#include <Arduino.h>
#include "esp_camera.h"
#include <WiFi.h>
#include <HTTPClient.h>
#include <WebServer.h>
#include <SPIFFS.h>
#include <base64.h>
#include <ESPmDNS.h>

// CAMERA_MODEL_AI_THINKER
#define PWDN_GPIO_NUM     32
#define RESET_GPIO_NUM    -1
#define XCLK_GPIO_NUM      0
#define SIOD_GPIO_NUM     26
#define SIOC_GPIO_NUM     27

#define Y9_GPIO_NUM       35
#define Y8_GPIO_NUM       34
#define Y7_GPIO_NUM       39
#define Y6_GPIO_NUM       36
#define Y5_GPIO_NUM       21
#define Y4_GPIO_NUM       19
#define Y3_GPIO_NUM       18
#define Y2_GPIO_NUM        5
#define VSYNC_GPIO_NUM    25
#define HREF_GPIO_NUM     23
#define PCLK_GPIO_NUM     22

const char index_html[] PROGMEM = R"rawliteral(
  <!DOCTYPE HTML><html>
  <head>
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
      body { text-align:center; }
      .vert { margin-bottom: 10%; }
      .hori{ margin-bottom: 0%; }
    </style>
  </head>
  <body>
    <div id="container">
      <h2>ESP32-CAM Last Photo</h2>
      <p>It might take more than 5 seconds to capture a photo.</p>
      <p>
        <button onclick="stopPhoto();">STOP PHOTO</button>
        <button onclick="capturePhoto()">CAPTURE PHOTO</button>
        <button onclick="location.reload();">REFRESH PAGE</button>
      </p>
    </div>
    <div><img src="/photo.jpg" id="photo" width="70%"></div>
  </body>
  <script>
    var deg = 0;
    function capturePhoto() {
      var xhr = new XMLHttpRequest();
      xhr.open('GET', "/capture", true);
      xhr.send();
    }
    function stopPhoto() {
      var xhr = new XMLHttpRequest();
      xhr.open('GET', "/stop", true);
      xhr.send();
    }
    function isOdd(n) { return Math.abs(n % 2) == 1; }
  </script>
  </html>)rawliteral";

WebServer server(80);
int captureDelay = 5000;

#define FILE_PHOTO "/photo.jpg"

bool captureEnabled = false;

IPAddress remoteServerIP;
uint16_t remoteServerPort = 0;
bool serviceFound = false;

// Fallback IP and port if discovery fails
const IPAddress fallbackIP(10, 0, 0, 1);
const uint16_t fallbackPort = 3131;

unsigned long discoveryStartTime = 0;
const unsigned long discoveryTimeout = 5000;

#define USB_MODE

#ifdef USB_MODE
HardwareSerial SerialUSB(0);
#endif

String fileToBase64(fs::FS &fs, const char * path) {
  File file = fs.open(path, "r");
  if (!file || file.isDirectory()) {
    Serial.println("Failed to open file");
    return "";
  }

  size_t size = file.size();
  Serial.printf("File size: %d bytes\n", size);

  // Allocate buffer
  uint8_t *buffer = (uint8_t*) malloc(size);
  if (!buffer) {
    Serial.println("Memory allocation failed");
    file.close();
    return "";
  }

  file.read(buffer, size);
  file.close();

  String encoded = base64::encode(buffer, size);
  free(buffer); // always free allocated memory

  return encoded;
}

String getContentType(String filename) {
  if (filename.endsWith(".html")) return "text/html";
  if (filename.endsWith(".css")) return "text/css";
  if (filename.endsWith(".js")) return "application/javascript";
  if (filename.endsWith(".png")) return "image/png";
  if (filename.endsWith(".jpg")) return "image/jpeg";
  if (filename.endsWith(".ico")) return "image/x-icon";
  return "text/plain";
}

void handleFileRequest() {
  if(!captureEnabled) {
    server.send(404);
    return;
  }
  String path = server.uri();
  if (path == "/") path = "/index.html";  // default file

  if (SPIFFS.exists(path)) {
    File file = SPIFFS.open(path, "r");
    String contentType = getContentType(path);
    server.streamFile(file, contentType);
    file.close();
  } else {
    server.send(404, "text/plain", "File Not Found");
  }
}

void handleRoot() {
  server.send(200, "text/html", index_html);
}

void capturePhoto() {
  captureEnabled = true;
}

void stopPhoto() {
  captureEnabled = false;
}

void savedPhoto() {
  server.send(200, "text/html", fileToBase64(SPIFFS, FILE_PHOTO));
}

void discoverCustomService() {
  int n = MDNS.queryService("custom", "tcp");
  Serial.printf("Found %d _custom._tcp services\n", n);

  for (int i = 0; i < n; i++) {
    Serial.printf("Service %d instance name: %s\n", i, MDNS.instanceName(i).c_str());

    if (MDNS.instanceName(i) == "ocrserver") {
      remoteServerIP = MDNS.address(i);
      remoteServerPort = MDNS.port(i);
      Serial.printf("Found ocrserver at %s:%u\n", remoteServerIP.toString().c_str(), remoteServerPort);
      serviceFound = true;
      return;
    }
  }
  // Not found in this query
  serviceFound = false;
}

String bytesToHex(const uint8_t *buffer, size_t len) {
  String hexString = "";
  for (size_t i = 0; i < len; i++) {
    char hex[3];
    sprintf(hex, "%02X", buffer[i]);
    hexString += hex;
  }
  return hexString;
}

void sendImageAsHex(const uint8_t *buffer, size_t len) {
  String hexString = bytesToHex(buffer, len);

  HTTPClient http;
  String url = "http://" + remoteServerIP.toString() + ":" + String(remoteServerPort) + "/ocr";
  http.begin(url); // Replace with your REST API URL
  http.addHeader("Content-Type", "application/json");

  String payload = "{\"image\": \"" + hexString + "\"}";

  int httpCode = http.POST(payload);

  if (httpCode > 0) {
    Serial.printf("HTTP POST response code: %d\n", httpCode);
  } else {
    Serial.printf("HTTP POST failed, error: %s\n", http.errorToString(httpCode).c_str());
  }

  http.end();
}

void sendImageRaw(const uint8_t *buffer, size_t len) {
  HTTPClient http;
  WiFiClient client;
  client.setTimeout(10000); // 10 seconds (default is 5000ms)

  String url = "http://" + remoteServerIP.toString() + ":" + String(remoteServerPort) + "/ocrraw";

  http.begin(client, url); // Replace with your REST API URL

  // Set the appropriate content type for your server
  http.addHeader("Content-Type", "application/octet-stream");

  // Send the raw image data as the POST body
  int httpCode = http.POST((uint8_t*)buffer, len);

  if (httpCode > 0) {
    Serial.printf("HTTP POST response code: %d\n", httpCode);
    String response = http.getString();
    Serial.println("Response body:");
    Serial.println(response);
  } else {
    Serial.printf("HTTP POST failed, error code: %d\n", httpCode);
    Serial.printf("Error string: %s\n", http.errorToString(httpCode).c_str());

    // Sometimes server might send error body even if httpCode < 0 (rare)
    String errorBody = http.getString();
    if (errorBody.length() > 0) {
      Serial.println("Error response body:");
      Serial.println(errorBody);
    }

    // Check if client is still connected
    if (!client.connected()) {
      Serial.println("Connection dropped or never established.");
    }
  }

  http.end();
}

void setup() {
  camera_config_t config;
  config.ledc_channel = LEDC_CHANNEL_0;
  config.ledc_timer = LEDC_TIMER_0;
  config.pin_d0 = Y2_GPIO_NUM;
  config.pin_d1 = Y3_GPIO_NUM;
  config.pin_d2 = Y4_GPIO_NUM;
  config.pin_d3 = Y5_GPIO_NUM;
  config.pin_d4 = Y6_GPIO_NUM;
  config.pin_d5 = Y7_GPIO_NUM;
  config.pin_d6 = Y8_GPIO_NUM;
  config.pin_d7 = Y9_GPIO_NUM;
  config.pin_xclk = XCLK_GPIO_NUM;
  config.pin_pclk = PCLK_GPIO_NUM;
  config.pin_vsync = VSYNC_GPIO_NUM;
  config.pin_href = HREF_GPIO_NUM;
  config.pin_sccb_sda = SIOD_GPIO_NUM;
  config.pin_sccb_scl = SIOC_GPIO_NUM;
  config.pin_pwdn = PWDN_GPIO_NUM;
  config.pin_reset = RESET_GPIO_NUM;
  config.xclk_freq_hz = 20000000;
  config.pixel_format = PIXFORMAT_JPEG;

  // init with high specs to pre-allocate larger buffers
  if(psramFound()){
    config.frame_size = FRAMESIZE_SVGA;
    config.jpeg_quality = 10;  //0-63 lower number means higher quality
    config.fb_count = 2;
  } else {
    config.frame_size = FRAMESIZE_CIF;
    config.jpeg_quality = 12;  //0-63 lower number means higher quality
    config.fb_count = 1;
  }  

  Serial.begin(115200);

#ifdef USB_MODE
  SerialUSB.begin(115200);
  if (esp_camera_init(&config) != ESP_OK) {
    Serial.println("Camera initialization failed");
    return;
  }
#else
  WiFi.begin("AAWirelessDongle", "password");

  // Wait for connection to WiFi
  while (WiFi.status() != WL_CONNECTED) {
    delay(1000);
    Serial.println("Connecting to WiFi...");
  }
  Serial.println("Connected to WiFi");
  if (!SPIFFS.begin(true)) {
    Serial.println("An Error has occurred while mounting SPIFFS");
    ESP.restart();
  }
  else {
    delay(500);
    Serial.println("SPIFFS mounted successfully");
  }
  Serial.print("IP Address: http://");
  Serial.println(WiFi.localIP());

  Serial.println("Starting webserver");
  server.onNotFound(handleFileRequest);  // Handle all unknown routes
  server.on("/", handleRoot);
  server.on("/capture", capturePhoto);
  server.on("/stop", stopPhoto);
  server.on("/saved-photo", savedPhoto);
  server.begin();
  Serial.println("Started webserver");

  // Initialize the camera
  if (esp_camera_init(&config) != ESP_OK) {
    Serial.println("Camera initialization failed");
    return;
  }

  discoveryStartTime = millis();
  discoverCustomService();
#endif
}

void loop() {
#ifdef USB_MODE
  camera_fb_t * fb = esp_camera_fb_get();
  if (!fb) {
    Serial.println("Camera capture failed");
    delay(captureDelay);
    return;
  }

  Serial.println("Captured image and sending");
  // Send start marker
  SerialUSB.write((const uint8_t*)"IMGSTART", 8);

  // Send image size as 4-byte little endian
  uint32_t size = fb->len;
  SerialUSB.write((uint8_t*)&size, 4);

  // Send image buffer
  SerialUSB.write(fb->buf, fb->len);

  // Send end marker
  SerialUSB.write((const uint8_t*)"IMGEND", 6);

  esp_camera_fb_return(fb);

  Serial.println("Finished sending");

  delay(captureDelay);
#else
  if (!serviceFound && (millis() - discoveryStartTime < discoveryTimeout)) {
    // Still within discovery window, keep trying
    discoverCustomService();
    delay(500);  // small delay to avoid spamming
  } 
  else if (!serviceFound && (millis() - discoveryStartTime >= discoveryTimeout)) {
    // Discovery timed out, use fallback
    remoteServerIP = fallbackIP;
    remoteServerPort = fallbackPort;
    serviceFound = true;
    Serial.printf("Using fallback IP: %s:%u\n", remoteServerIP.toString().c_str(), remoteServerPort);
  }

  if (serviceFound) {
    camera_fb_t *fb = esp_camera_fb_get();
    if (!fb) {
      Serial.println("Camera capture failed");
      return;
    }

    //sendImageAsHex(fb->buf, fb->len);
    sendImageRaw(fb->buf, fb->len);

    if(captureEnabled) {
      File file = SPIFFS.open(FILE_PHOTO, FILE_WRITE);
      // Insert the data in the photo file
      if (!file) {
        Serial.println("Failed to open file in writing mode");
      }
      else {
        file.write(fb->buf, fb->len); // payload (image), payload length
        Serial.print("The picture has been saved in ");
        Serial.print(FILE_PHOTO);
        Serial.print(" - Size: ");
        Serial.print(file.size());
        Serial.println(" bytes");
      }
      // Close the file
      file.close();
    }

    esp_camera_fb_return(fb);

    delay(captureDelay); // Take a picture every 5 seconds
  }
#endif
}
