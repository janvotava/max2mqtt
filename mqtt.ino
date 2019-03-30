
void mqttReconnect()
{
  if (!client.connected())
  {
    Serial.print("Attempting MQTT connection...");
    // Create a random client ID
    String clientId = "max-";
    clientId += String(random(0xffff), HEX);
    // Attempt to connect
    if (client.connect(clientId.c_str(), "max", 1, true, "{\"availability\":\"offline\"}"))
    {
      Serial.println("connected");
      // ... and resubscribe
      client.publish("max", "{\"availability\":\"online\"}", true);
      client.subscribe("max/rename", 1);
      client.subscribe("max/save-config", 1);
      client.subscribe("max/format", 1);
      client.subscribe("max/reset", 1);
      client.subscribe("max/set", 1);

      subscribeToDeviceSetTopics();
    }
    else
    {
      Serial.print("failed, rc=");
      Serial.println(client.state());
    }
  }
}

void mqttLoop()
{
  static unsigned long last_mqtt_reconnect = millis();
  if (!client.connected() && WiFi.status() == WL_CONNECTED && millis() - last_mqtt_reconnect > 1000)
  {
    mqttReconnect();
    last_mqtt_reconnect = millis();
  }
  client.loop();
}