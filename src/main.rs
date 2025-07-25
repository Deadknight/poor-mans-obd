use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::{io::Cursor, sync::Arc, path::PathBuf};
use std::error::Error;

use libmdns::{Responder, Service};

use serialport::SerialPort;
use std::time::Duration;
use std::io::{Read, Write};
use std::fs::File;

use axum::body::Bytes;
use axum::routing::get;
use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;
use axum::{
    Router,
    extract::{Json, State},
    response::IntoResponse,
    routing::post,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use hex::decode;
use tokio::net::TcpListener;
use image::{ImageReader, RgbImage};
use tokio::task::spawn_blocking;
use tokio::sync::mpsc;

#[derive(Clone)]
struct AppState {
    ocrengine: Arc<OcrEngine>,
    latest_uploaded_image: Arc<Mutex<Option<Vec<u8>>>>, // always holds latest image to serve
    ocr_queue: Arc<Mutex<Option<Vec<u8>>>>,   
    ocr_running: Arc<AtomicBool>,
    serial_tx: mpsc::Sender<Vec<u8>>,
}

#[derive(Deserialize)]
struct ImagePayload {
    image: String,
}

#[derive(Serialize)]
pub struct BatteryData {
    pub battery_level_percentage: Option<f32>,
    pub battery_level_wh: Option<u64>,
    pub battery_capacity_wh: Option<u64>,
    pub reference_air_density: Option<f32>,
    pub external_temp_celsius: Option<f32>,
}

fn handle_raw_image(state: &AppState, image_data: Vec<u8>) {
    {
        let mut latest = state.latest_uploaded_image.lock().unwrap();
        *latest = Some(image_data.clone());
    }

    {
        let mut queue = state.ocr_queue.lock().unwrap();
        *queue = Some(image_data);
    }

    spawn_ocr_task(state.clone());
}

async fn receive_image_raw(
    State(state): State<AppState>,
    body: Bytes
) -> Result<impl IntoResponse, (StatusCode, String)> {
    println!("Received raw image upload, size: {} bytes", body.len());

    handle_raw_image(&state, body.to_vec());

    Ok((StatusCode::ACCEPTED, "Image saved and queued for OCR".to_string()))
}


async fn receive_image(
    State(state): State<AppState>,
    Json(payload): Json<ImagePayload>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let bytes = decode(payload.image.trim())
        .map_err(|e| {
            let msg = format!("Invalid hex: {}", e);
            println!("{}", msg);
            (StatusCode::BAD_REQUEST, msg)
        })?;

    {
        let mut uploaded = state.latest_uploaded_image.lock().unwrap();
        *uploaded = Some(bytes.clone()); // save to serve
    }

    {
        let mut queue = state.ocr_queue.lock().unwrap();
        *queue = Some(bytes); // overwrite OCR queue with latest image
    }

    spawn_ocr_task(state.clone());

    Ok((StatusCode::ACCEPTED, "Image saved and queued for OCR".to_string()))
}

fn spawn_ocr_task(state: AppState) {
    println!("OCR task spawn requested");
    // If already running, skip spawning a new one
    if state.ocr_running.swap(true, Ordering::SeqCst) {
        println!("OCR already running, skipping spawn");
        return;
    }

    tokio::spawn(async move {
        println!("OCR task started");
        loop {
            let maybe_image = {
                let mut lock = state.ocr_queue.lock().unwrap();
                lock.take()
            };

            match maybe_image {
                Some(image_bytes) => {
                    println!("OCR got image to process, {} bytes", image_bytes.len());

                    let engine = state.ocrengine.clone();
                    let result = spawn_blocking(move || {
                        // Save image for debug
                        /*if let Err(e) = std::fs::write("debug_image.jpg", &image_bytes) {
                            eprintln!("Failed to save debug image: {}", e);
                        } else {
                            println!("Saved debug image as debug_image.jpg");
                        }*/

                        let img = ImageReader::new(Cursor::new(image_bytes))
                            .with_guessed_format()
                            .map_err(|e| format!("Image format error: {}", e))?
                            .decode()
                            .map_err(|e| format!("Image decode error: {}", e))?
                            .into_rgb8();

                        println!("Image decoded successfully, starting OCR");

                        ocr_image(&engine, img).map_err(|e| format!("OCR failed: {}", e))
                    })
                    .await;

                    match result {
                        Ok(Ok(())) => {
                            println!("OCR completed successfully");
                        }
                        Ok(Err(e)) => {
                            eprintln!("OCR processing error: {}", e);
                        }
                        Err(e) => {
                            eprintln!("OCR task join error: {:?}", e);
                        }
                    }
                }
                None => {
                    println!("No images left in OCR queue, stopping OCR task");
                    state.ocr_running.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }
        println!("OCR task exited");
    });
}

async fn get_latest_image(State(state): State<AppState>) -> impl IntoResponse {
    let img_data = {
        let lock = state.latest_uploaded_image.lock().unwrap();
        lock.clone()
    };

    if let Some(img) = img_data {
        let mime = infer::get(&img)
            .map(|t| t.mime_type())
            .unwrap_or("application/octet-stream");

        (
            [(axum::http::header::CONTENT_TYPE, mime)],
            img,
        )
            .into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            "No image has been uploaded yet.".to_string(),
        )
            .into_response()
    }
}
 
fn ocr_image(engine: &OcrEngine, img: RgbImage) -> Result<(), Box<dyn Error>> {
    let img_source = ImageSource::from_bytes(img.as_raw(), img.dimensions())?;
    let ocr_input = engine.prepare_input(img_source)?;
    let word_rects = engine.detect_words(&ocr_input)?;
    let line_rects = engine.find_text_lines(&ocr_input, &word_rects);
    let line_texts = engine.recognize_text(&ocr_input, &line_rects)?;

    println!("New detection");
    println!("");

    for line in line_texts.iter().flatten().filter(|l| l.to_string().len() > 1) {
        if(line.to_string().contains('%')) {
            if let Some(first_value) = line.to_string().split('%').next() {
                if let Some(substr) = first_value.get(1..) {
                    println!("Detected: {}", substr);

                    // Extract only digits (no decimal point)
                    let digits_only: String = substr.chars()
                    .filter(|c| c.is_digit(10))
                    .collect();

                    println!("Filtered digits only: {}", digits_only);

                    if let Ok(battery_level) = digits_only.parse::<f32>() {
                        if (0.0..=100.0).contains(&battery_level) {
                            // Valid battery percentage, proceed to send
                            tokio::spawn(async move {
                                if let Err(e) = send_battery_data(battery_level).await {
                                    eprintln!("Failed to send battery data: {}", e);
                                }
                            });
                        } else {
                            eprintln!("Battery percentage out of range (0-100): {}", battery_level);
                        }
                    } else {
                        eprintln!("Failed to parse battery percentage from '{}'", digits_only);
                    }
                }
            }
        }
    }

    println!("New detection end");
    println!("");

    Ok(())
}

// Async function to send battery data
async fn send_battery_data(battery_percentage: f32) -> Result<(), reqwest::Error> {
    let data = BatteryData {
        battery_level_percentage: Some(battery_percentage),
        battery_level_wh: None,
        battery_capacity_wh: None,
        reference_air_density: None,
        external_temp_celsius: None,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post("http://127.0.0.1:80/battery") // change host if needed
        .json(&data)
        .send()
        .await?;

    if resp.status().is_success() {
        println!("Battery data sent successfully");
    } else {
        eprintln!("Failed to send battery data: HTTP {}", resp.status());
    }

    Ok(())
}

fn file_path(path: &str) -> PathBuf {
    let mut abs_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    abs_path.push(path);
    abs_path
}

async fn image_viewer_page() -> impl IntoResponse {
    let html = r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Live Image Viewer</title>
            <style>
                body {
                    background-color: #111;
                    color: #fff;
                    font-family: sans-serif;
                    text-align: center;
                }
                img {
                    max-width: 90%;
                    border: 2px solid #444;
                    margin-top: 20px;
                }
            </style>
        </head>
        <body>
            <h1>Live Image Feed</h1>
            <img id="liveImage" src="/latest-image" />
            <script>
                setInterval(() => {
                    const img = document.getElementById('liveImage');
                    img.src = '/latest-image?' + new Date().getTime(); // prevent caching
                }, 1000); // every second
            </script>
        </body>
        </html>
    "#;

    (StatusCode::OK, [("Content-Type", "text/html")], html)
}

use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let detection_model_path = file_path("/persist/poor_mans_obd/text-detection.rten");
    let rec_model_path = file_path("/persist/poor_mans_obd/text-recognition.rten");

    let detection_model = Model::load_file(detection_model_path)?;
    let recognition_model = Model::load_file(rec_model_path)?;

    let engine = Arc::new(OcrEngine::new(OcrEngineParams {
        detection_model: Some(detection_model),
        recognition_model: Some(recognition_model),
        ..Default::default()
    })?);

    let (serial_tx, mut serial_rx) = mpsc::channel::<Vec<u8>>(10);

    let state = AppState {
        ocrengine: engine.clone(),
        latest_uploaded_image: Arc::new(Mutex::new(None)),
        ocr_queue: Arc::new(Mutex::new(None)),
        ocr_running: Arc::new(AtomicBool::new(false)),
        serial_tx: serial_tx.clone(),
    };

    // Spawn an async task to receive images from the serial thread and handle them async:
    let state_for_task = state.clone();
    tokio::spawn(async move {
        while let Some(image_data) = serial_rx.recv().await {
            handle_raw_image(&state_for_task, image_data);
        }
    });

    {
        let state = state.clone();
        let serial_tx = state.serial_tx.clone();
        std::thread::spawn(move || {
            use std::thread;
            use std::time::Duration;
            use serialport::SerialPort;
    
            fn read_until(port: &mut dyn SerialPort, pattern: &[u8]) -> std::io::Result<()> {
                let mut buffer = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    port.read_exact(&mut byte)?;
                    buffer.push(byte[0]);
                    if buffer.ends_with(pattern) {
                        return Ok(());
                    }
                    if buffer.len() > pattern.len() {
                        buffer.remove(0);
                    }
                }
            }
    
            fn read_image(port: &mut dyn SerialPort) -> std::io::Result<Vec<u8>> {
                read_until(port, b"IMGSTART")?;
    
                let mut size_buf = [0u8; 4];
                port.read_exact(&mut size_buf)?;
                let size = u32::from_le_bytes(size_buf) as usize;
    
                let mut img_buf = vec![0u8; size];
                port.read_exact(&mut img_buf)?;
    
                read_until(port, b"IMGEND")?;
    
                Ok(img_buf)
            }
    
            loop {
                match serialport::new("/dev/ttyUSB0", 115_200)
                    .timeout(Duration::from_secs(10))
                    .open()
                {
                    Ok(mut port) => {
                        println!("Serial port /dev/ttyUSB0 opened");
                        loop {
                            match read_image(&mut *port) {
                                Ok(image_data) => {
                                    println!("Serial: received image ({} bytes)", image_data.len());
                                    // Send image into async context (blocks if channel is full)
                                    if let Err(e) = serial_tx.blocking_send(image_data) {
                                        eprintln!("Failed to send image via channel: {}", e);
                                        break; // Exit inner loop to reopen port
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Serial read error: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Waiting for /dev/ttyUSB0: {}", e);
                        thread::sleep(Duration::from_secs(5)); // Wait before retrying
                    }
                }
            }
        });
    }

    // Start mDNS responder
    let responder = Responder::new().expect("Failed to create mDNS responder");

    // Register "ocrserver.local" with _ocr._tcp.local service on port 3030
    let _svc = responder.register(
        "_custom._tcp".to_string(),  // This lets the ESP32 find it via HTTP too
        "ocrserver".to_string(),   // Hostname = ocrserver.local
        3131,
        &[],
    );

    println!("mDNS service registered as ocrserver.local on port 3131");

    let app = Router::new()
        .route("/ocr", post(receive_image))
        .route("/ocrraw", post(receive_image_raw))
        .route("/latest-image", get(get_latest_image))
        .route("/viewer", get(image_viewer_page))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let server_ip = "0.0.0.0:3131";

    println!("Server started at {}", server_ip);

    let listener = TcpListener::bind("0.0.0.0:3131").await.unwrap();
    axum::serve(listener, app).await.unwrap();

    Ok(())
}