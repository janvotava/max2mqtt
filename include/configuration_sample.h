#define WIFI_SSID "mywifi"
#define WIFI_PASSWORD "password"
#define HOSTNAME "max"
#define MQTT_SERVER "mqtt.lan"

// Advanced
#define BURNER_RELAY_PIN D1
#define CC1101_IRQ_PIN D2
#define CREDIT_15MIN 36 / 4 * 1000 // We can communicate 36s per hour, split to 9s chunks per 15 minutes
