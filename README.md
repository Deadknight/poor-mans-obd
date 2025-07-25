# poor-mans-obd

MB Cars have their obd communication encrypted. This solves issue by using ocr to get battery percentage from infotaintment

This works with [aa-proxy-rs](https://github.com/aa-proxy/aa-proxy-rs).

ESP32Cam reads data and sends it through usb to pi4.
Companion app ocr image in pi4 and sends battery data to aa-proxy-rs.

You have to enable 
"modprobe ch341" in init.d

To build use:
docker build --target export --output type=local,dest=./output .
