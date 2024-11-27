/*
  CC11xx pins    ESP pins   Description
  1 - VCC        VCC        3v3
  2 - GND        GND        Ground
  3 - MOSI       13=D7      Data input to CC11xx
  4 - SCK        14=D5      Clock pin
  5 - MISO/GDO1  12=D6      Data output from CC11xx / serial clock from CC11xx
  6 - GDO2       04=D2      Serial data to CC11xx
  8 - CSN        15=D8      Chip select / (SPI_SS)
*/

#include "Arduino.h"
#include <SPI.h>
// #include "MaxCC1101.h"
#include "MaxCC1101.h"
#include <ESP8266WiFi.h>
#include <ESP8266WiFiMulti.h>
#include <ArduinoJson.h>
#include <ArduinoOTA.h>
#include <DNSServer.h>
#include <ESP8266mDNS.h>
#include <PubSubClient.h>
#include <vector>
#include <queue>
#include "max.h"
#include "state.h"
#include "message.h"
#include "configuration.h"
#include "time.hpp"
#include "mqtt.hpp"
#include "config.hpp"
#include "main.hpp"

WiFiClient espClient;
PubSubClient client(espClient);
// client = PubSubClient(espClient);
// client(espClient);

MaxCC1101 rf;

byte msgCounter = 0;

#define Debug Serial

const int capacity PROGMEM = JSON_OBJECT_SIZE(10) + 256;

bool pairing_enabled = false;
bool autocreate = true;
// extern bool autocreate = true;
unsigned int last_config_changed = millis();
bool config_changed = false;
bool published_started_at_state = false;

bool furnace_running = false;
char boot_time[20];

byte myAddress[3] = {0x12, 0x34, 0x56};
std::vector<state> states;
std::queue<Message> queue;
std::queue<CC1101Packet> received_messages;

String bootedAt;

void startBurner()
{
#ifdef BURNER_RELAY_PIN
  digitalWrite(BURNER_RELAY_PIN, HIGH);
#endif
}

void stopBurner()
{
#ifdef BURNER_RELAY_PIN
  digitalWrite(BURNER_RELAY_PIN, LOW);
#endif
}

ESP8266WiFiMulti wifiMulti;

void setup_wifi()
{
  delay(10);

  Serial.println();
  Serial.print("Connecting to ");
  Serial.println(WIFI_SSID);

  WiFi.softAPdisconnect(true);
  wifiMulti.addAP(WIFI_SSID, WIFI_PASSWORD);
  WiFi.hostname(HOSTNAME);

  randomSeed(micros());

  Serial.println("");
  Serial.println("WiFi connected");
  Serial.println("IP address: ");
  Serial.println(WiFi.localIP());

  if (MDNS.begin(HOSTNAME))
  {
    Serial.print("* MDNS responder started. Hostname -> ");
    Serial.println(HOSTNAME);
  }
}

#ifdef CREDIT_15MIN
unsigned long creditMs = CREDIT_15MIN;
#endif

// 1kb/s = 1 bit/ms. we send 1 sec preamble + len*8 bits
bool takeFromCredit(byte length, bool preamble = false)
{
#ifndef CREDIT_15MIN
  return true;
#endif

#ifdef CREDIT_15MIN
  short creditRequired = length * 8;
  if (preamble)
  {
    creditRequired += 1000;
  }

  if (creditRequired < creditMs)
  {
    creditMs -= creditRequired;
    return true;
  }

  return false;
#endif
}

void send(CC1101Packet *packet, bool preamble)
{
  char buffer[packet->length * 2 + 1];
  bytesToString(buffer, packet->data, packet->length);

  Debug.print("Sending");
  if (preamble)
  {
    Debug.print(" with long preamble");
  }
  Debug.printf(": %s\n", buffer);
  rf.sendData(packet, preamble);
  Debug.println("Done.");
}

void sendMessageFromQueue()
{
  if (queue.empty())
  {
    return;
  }

  static bool locked = false;
  if (!locked)
  {
    locked = true;
    Message *message;
    message = &queue.front();

    if (takeFromCredit(message->packet.length, message->longPreamble))
    {
      send(&message->packet, message->longPreamble);
      message->sent = true;
      message->longPreamble = true; // If we don't get ACK on short preamble, retry with long one.

      if (!message->waitForAck)
      {
        queue.pop();
      }
      else if (message->retryCounter++ >= 4)
      {
        // Bail out.
        queue.pop();
      }
      delay(200);
    }
    else
    {
      if (!message->waitForAck)
      {
        queue.pop();
        Debug.println("Out of credit, not sending. Tossing message away, because not marked as wait for ACK.");
      }
    }
    locked = false;
  }
}

void ackMessageInQueue(byte msgcnt)
{
  Message *message = &queue.front();

  if (message->sent && message->waitForAck && message->msgcnt == msgcnt)
  {
    Debug.print(", ACKed message in the queue");
    queue.pop();
  }
}

unsigned int stringToBytes(byte *data, const char *payload, unsigned int length)
{
  char tmp[3];
  tmp[2] = '\0';
  int i = 0;
  int j = 0;
  int len = length;
  long n;
  for (i = 0; i < len; i += 2)
  {
    tmp[0] = payload[i];
    tmp[1] = payload[i + 1];
    // tmp now has two char hex value
    n = strtol(tmp, NULL, 16); // n has numeric value

    data[j] = n;
    j++;
  }

  return j;
}

state *findDeviceByAddress(byte *address)
{
  state *device;
  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];

    // Find existing device state or select a new one
    if ((device->address[0] == address[0] &&
         device->address[1] == address[1] &&
         device->address[2] == address[2]))
    {
      return device;
    }
  }

  return 0;
}

state *findDeviceByName(String name)
{
  if (name == "")
  {
    return 0;
  }

  state *device;
  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];

    if (device->name == name)
    {
      return device;
    }
  }

  return 0;
}

void addToQueue(CC1101Packet packet, bool longPreamble = true, bool waitForAck = false)
{
  Message message;
  message.sent = false;
  message.packet = packet;
  message.longPreamble = longPreamble;
  message.waitForAck = waitForAck;
  message.retryCounter = 0;
  message.msgcnt = packet.data[1];
  queue.push(message);
}

bool validateAddress(const char *address)
{
  return address && strlen(address) == 6;
}

void rename(byte *payload)
{
  StaticJsonDocument<200> doc;

  DeserializationError error = deserializeJson(doc, payload);

  if (error)
  {
    Debug.print("Rename deserializeJson() failed: ");
    Debug.println(error.c_str());
    return;
  }

  const char *address = doc["address"];

  if (!validateAddress(address))
  {
    Debug.println("Not an address");
    return;
  }

  byte from[3];
  stringToBytes(from, address, 6);

  state *device = findDeviceByAddress(from);

  if (device)
  {
    Debug.println("Found device to rename...");
    JsonObject root = doc.as<JsonObject>();
    String newName = root["to"];

    if (root.containsKey("to") && newName != device->name)
    {
      device->name = newName;
      String topic = "max/";
      topic += device->name;
      topic += "/set";

      Debug.printf("Subscribing to: %s\n", topic.c_str());
      client.subscribe(topic.c_str(), 1);
      config_changed = true;
    }
  }
}

void setRoom(state *device, String room)
{
  if (room != device->room)
  {
    device->room = room;
    config_changed = true;
    Debug.printf("Assigning room %s\n", room.c_str());
  }
}

void setGroup(state *device, byte group)
{
  CC1101Packet outMessage;
  outMessage.data[0] = 11; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = 0;
  outMessage.data[3] = SET_GROUP_ID_CMD;

  for (int i = 0; i < 3; ++i) /* src = enc_dst*/
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i) /* dst = enc_src */
    outMessage.data[7 + i] = device->address[i];

  outMessage.data[10] = 0;     // groupId
  outMessage.data[11] = group; // new group id
  outMessage.length = 12;

  Debug.printf("Assigning group for %s to ", device->name.c_str());
  Debug.println(group);
  addToQueue(outMessage, true, true);

  if (group != device->group)
  {
    device->group = group;
    config_changed = true;
  }
}

void setDesiredTemperature(state *device, int mode, float temperature)
{
  CC1101Packet outMessage;
  outMessage.data[0] = 11; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = device->group == 0 ? 0 : 4; //msgflag - 0, when groupid present 04
  outMessage.data[3] = SET_TEMPERATURE_CMD;

  for (int i = 0; i < 3; ++i) /* src = enc_dst*/
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i) /* dst = enc_src */
    outMessage.data[7 + i] = device->address[i];

  outMessage.data[10] = device->group; // GroupId
  outMessage.data[11] = (int)(temperature * 2) | (mode << 6);
  outMessage.length = 12;

  Debug.printf("Setting desired temperature of %s to ", device->name.c_str());
  Debug.println(temperature);

  addToQueue(outMessage, true, true);
}

// 0 = Saturday
// 1 = Sunday
// 2 = Monday
// 3 = Tuesday
// 4 = Wednesday
// 5 = Thursday
// 6 = Friday
const char *DAYS[] PROGMEM = {
    "saturday",
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
};

byte stringToEq3Day(String day)
{
  day.toLowerCase();
  day.trim();

  for (int i = 0; i < 7; i++)
  {
    if (day == DAYS[i])
    {
      return i;
    }
  }

  return UNDEFINED;
}

void sendScheduleTo(state *device, byte weekDay, byte *schedule, byte size)
{
  CC1101Packet outMessage;
  outMessage.data[0] = 11 + size; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = device->group == 0 ? 0 : 4; //msgflag - 0, when groupid present 04
  outMessage.data[3] = CONFIG_WEEK_PROFILE_CMD;

  for (int i = 0; i < 3; ++i)
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i)
    outMessage.data[7 + i] = device->address[i];

  outMessage.data[10] = device->group; // GroupId
  outMessage.data[11] = weekDay;
  memcpy(outMessage.data + 12, schedule, size);

  outMessage.length = 12 + size;

  Debug.printf("Setting schedule for %s\n", device->name.c_str());

  addToQueue(outMessage, true, true);
}

void setSchedule(state *device, String day, JsonObject schedule_config)
{
  const int count = schedule_config.size();
  byte schedule[count * 2];
  const byte weekDay = stringToEq3Day(day);

  if (device->schedule[weekDay] != 0)
  {
    delete[] device->schedule[weekDay];
  }

  if (count > 0)
  {
    device->schedule[weekDay] = new byte[count * 2];

    byte idx = 0;
    for (JsonPair p : schedule_config)
    {
      String hoursMinutes = p.key().c_str();
      float temperature = p.value().as<float>();
      const int temperatureInt = temperature * 2;
      const int colonPos = hoursMinutes.indexOf(":");
      if (!colonPos)
      {
        continue;
      }

      short h = hoursMinutes.substring(0, colonPos).toInt();
      short m = hoursMinutes.substring(colonPos + 1, hoursMinutes.length()).toInt();

      int minuteChunksFromMidnight = (h * 60 + m) / 5;
      short temperatureWithUntil = temperatureInt << 9 | minuteChunksFromMidnight;
      schedule[idx] = highByte(temperatureWithUntil);
      schedule[idx + 1] = lowByte(temperatureWithUntil);

      const byte state_schedule_idx = weekDay * DAY_SCHEDULE_LENGTH + idx;

      device->schedule[weekDay][idx] = highByte(temperatureWithUntil);
      device->schedule[weekDay][idx + 1] = lowByte(temperatureWithUntil);

      idx += 2;
    }
  }
  device->schedule_size[weekDay] = count * 2;

  config_changed = true;
  sendScheduleTo(device, weekDay, schedule, count * 2);
}

bool hasAssociation(state *device, byte *address)
{
  for (byte pos = 0; pos < device->associated_devices.size(); pos += 3)
  {
    if (memcmp(device->associated_devices.data() + pos, address, 3) == 0)
    {
      return true;
    }
  }
  return false;
}

void addAssociation(state *device, byte *address)
{
  if (!hasAssociation(device, address))
  {
    device->associated_devices.insert(device->associated_devices.end(), address, address + 3);
    config_changed = true;
  }
}

void addLinkPartner(byte *address, byte *to, byte type)
{
  CC1101Packet outMessage;
  outMessage.data[0] = 14; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = 0; //msgflag
  outMessage.data[3] = ADD_LINK_PARTNER_CMD;
  for (int i = 0; i < 3; ++i)
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i)
    outMessage.data[7 + i] = address[i];
  outMessage.data[10] = 0; // GroupId
  for (int i = 0; i < 3; ++i)
    outMessage.data[11 + i] = to[i];
  outMessage.data[14] = type;
  outMessage.length = 15;

  Debug.printf("Asociating to link partner type %i\n", type);

  addToQueue(outMessage, true, true);
}

void sendAssociateBetween(state *device, state *toDevice)
{
  if (toDevice->type != UNDEFINED)
  {
    addLinkPartner(device->address, toDevice->address, toDevice->type);
  }

  if (device->type != UNDEFINED)
  {
    addLinkPartner(toDevice->address, device->address, device->type);
  }
}

void associate(state *device, String to)
{
  state *toDevice = findDeviceByName(to);
  if (toDevice)
  {
    sendAssociateBetween(device, toDevice);
    addAssociation(device, toDevice->address);
    addAssociation(toDevice, device->address);
  }
}

// comfort, eco, max, min, window open
void setTemperatureSettings(state *device, float comfort, float eco, float max, float min, float window_open)
{
  if (
      (comfort != device->comfort_temperature) ||
      (eco != device->eco_temperature) ||
      (max != device->max_temperature) ||
      (min != device->min_temperature) ||
      (window_open != device->window_open_temperature))
  {
    device->comfort_temperature = comfort;
    device->eco_temperature = eco;
    device->max_temperature = max;
    device->min_temperature = min;
    device->window_open_temperature = window_open;

    config_changed = true;
  }

  CC1101Packet outMessage;
  outMessage.data[0] = 17; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = 0; //msgflag
  outMessage.data[3] = CONFIG_TEMPERATURES_CMD;
  for (int i = 0; i < 3; ++i)
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i)
    outMessage.data[7 + i] = device->address[i];
  outMessage.data[10] = 0; // GroupId
  outMessage.data[11] = comfort * 2;
  outMessage.data[12] = eco * 2;
  outMessage.data[13] = max * 2;
  outMessage.data[14] = min * 2;
  outMessage.data[15] = 7; // offsset = 0 [7 -> 7/2 - 3.5 = 0]
  outMessage.data[16] = window_open * 2;
  outMessage.data[17] = 3; // window open = 15 min [3 -> 3*5 -> 15 (minutes)]
  outMessage.length = 18;

  Debug.printf("Setting temperatures for %s to eco: ", device->name.c_str());
  Debug.println(eco);

  addToQueue(outMessage, true, true);
}

void setDisplayActualTemperatureState(state *device, bool display_actual_temperature)
{
  if (display_actual_temperature != device->display_actual_temperature)
  {
    config_changed = true;
  }
  device->display_actual_temperature = display_actual_temperature;
}

void configValveFunctions(state *device, byte decalc_weekday = 0, byte decalc_hour = 12, byte boost_duration = 6, byte boost_valve_position = 100, byte max_valve_setting = 100, byte valve_offset = 0)
{
  if (
      (decalc_weekday != device->decalc_weekday) ||
      (decalc_hour != device->decalc_hour) ||
      (boost_duration != device->boost_duration) ||
      (boost_valve_position != device->boost_valve_position) ||
      (max_valve_setting != device->max_valve_setting) ||
      (valve_offset != device->valve_offset))
  {
    device->decalc_weekday = decalc_weekday;
    device->decalc_hour = decalc_hour;
    device->boost_duration = boost_duration;
    device->boost_valve_position = boost_valve_position;
    device->max_valve_setting = max_valve_setting;
    device->valve_offset = valve_offset;

    config_changed = true;
  }

  CC1101Packet outMessage;
  outMessage.data[0] = 14; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = 0; //msgflag
  outMessage.data[3] = CONFIG_VALVE_CMD;
  for (int i = 0; i < 3; ++i)
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i)
    outMessage.data[7 + i] = device->address[i];
  outMessage.data[10] = 0; // GroupId

  outMessage.data[11] = (boost_duration << 5) | (boost_valve_position / 5);
  outMessage.data[12] = (decalc_weekday << 5) | decalc_hour;
  outMessage.data[13] = max_valve_setting * 255 / 100;
  outMessage.data[14] = valve_offset * 255 / 100;
  outMessage.length = 15;

  Debug.printf("Setting valve config for %s\n", device->name.c_str());

  addToQueue(outMessage, true, true);
}

void displayActualTemperature(state *device, bool isEnabled)
{
  CC1101Packet outMessage;
  outMessage.data[0] = 11; // Length
  outMessage.data[1] = msgCounter++;
  outMessage.data[2] = 0; //msgflag
  outMessage.data[3] = SET_DISPLAY_ACTUAL_TEMPERATURE_CMD;
  for (int i = 0; i < 3; ++i)
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i)
    outMessage.data[7 + i] = device->address[i];
  outMessage.data[10] = 0; // GroupId
  outMessage.data[11] = isEnabled ? 4 : 0;
  outMessage.length = 12;

  Debug.printf("Setting display actual temperature for %s to %i\n", device->name.c_str(), isEnabled);
  setDisplayActualTemperatureState(device, isEnabled);

  addToQueue(outMessage, true, true);
}

void setAddress(const char *address)
{
  if (validateAddress(address))
  {
    Debug.printf("Changing address to: %s\n", address);
    stringToBytes(myAddress, address, 6);
    config_changed = true;
  }
  else
  {
    Debug.printf("Address invalid: %s\n", address);
  }
}

void setSelf(byte *payload)
{
  StaticJsonDocument<200> doc;

  DeserializationError error = deserializeJson(doc, payload);

  if (error)
  {
    Debug.println("Cannot parse payload.");
    return;
  }

  JsonObject root = doc.as<JsonObject>();

  for (JsonPair p : root)
  {
    JsonString key = p.key();
    JsonVariant value = p.value();

    if (key == "address")
    {
      setAddress(value);
    }
    else if (key == "pair")
    {
      pairing_enabled = value;
      Debug.printf("Setting pairing enabled to %s\n", pairing_enabled ? "true" : "false");
      publishState();
    }
    else if (key == "autocreate")
    {
      autocreate = value;
      Debug.printf("Setting autocreate to %s\n", autocreate ? "true" : "false");
      config_changed = true;
      publishState();
    }
  }
}

void publishState()
{
  StaticJsonDocument<capacity> doc;
  char output[128];
  doc["availability"] = "online";
  doc["booted_at"] = bootedAt;
  doc["pairing_enabled"] = pairing_enabled;
  doc["autocreate"] = autocreate;
  doc["furnace_running"] = furnace_running;

  serializeJson(doc, output);
  if (client.publish("max", output, true))
  {
    published_started_at_state = true;
  }
}

void set(state *device, byte *payload)
{
  StaticJsonDocument<200> doc;

  DeserializationError error = deserializeJson(doc, payload);

  if (error)
  {
    Serial.println("Cannot parse payload.");
    return;
  }

  JsonObject root = doc.as<JsonObject>();
  int mode = device->mode || MODE_MANUAL;

  if (root.containsKey("mode"))
  {
    mode = stringToMode(root["mode"]);
  }

  if (
      root.containsKey("eco_temperature") ||
      root.containsKey("comfort_temperature") ||
      root.containsKey("max_temperature") ||
      root.containsKey("min_temperature") ||
      root.containsKey("window_open_temperature"))
  {
    float eco_temperature = root["eco_temperature"] | device->eco_temperature;
    float comfort_temperature = root["comfort_temperature"] | device->comfort_temperature;
    float max_temperature = root["max_temperature"] | device->max_temperature;
    float min_temperature = root["min_temperature"] | device->min_temperature;
    float window_open_temperature = root["window_open_temperature"] | device->window_open_temperature;

    setTemperatureSettings(device, comfort_temperature, eco_temperature, max_temperature, min_temperature, window_open_temperature);
  }

  if (
      root.containsKey("decalc_weekday") ||
      root.containsKey("decalc_hour") ||
      root.containsKey("boost_duration") ||
      root.containsKey("boost_valve_position") ||
      root.containsKey("max_valve_setting") ||
      root.containsKey("valve_offset"))
  {
    byte decalc_weekday = device->decalc_weekday;

    if (root.containsKey("decalc_weekday"))
    {
      byte weekday = stringToEq3Day(root["decalc_weekday"]);

      decalc_weekday = weekday || decalc_weekday;
    }

    // Convert boost duration to 5 minut chunks
    // 5 = 25 min
    // 6 = 30 min
    // 7 = 60 min
    byte boost_duration_chunks = device->boost_duration;
    if (root.containsKey("boost_duration"))
    {
      byte boost_duration = root["boost_duration"];
      boost_duration_chunks = boost_duration / 5;
      if (boost_duration_chunks > 7)
      {
        boost_duration_chunks = 7;
      }
    }

    byte decalc_hour = root["decalc_hour"] | device->decalc_hour;
    byte boost_valve_position = root["boost_valve_position"] | device->boost_valve_position;
    byte max_valve_setting = root["max_valve_setting"] | device->max_valve_setting;
    byte valve_offset = root["valve_offset"] | device->valve_offset;

    configValveFunctions(device, decalc_weekday, decalc_hour, boost_duration_chunks, boost_valve_position, max_valve_setting, valve_offset);
  }

  for (JsonPair p : root)
  {
    JsonString key = p.key();
    JsonVariant value = p.value();

    if (key == "room")
    {
      setRoom(device, value);
    }
    else if (key == "group")
    {
      setGroup(device, value);
    }
    else if (key == "schedule")
    {
      if (root.containsKey("day"))
      {
        setSchedule(device, root["day"], value);
      }
    }
    else if (key == "temperature" || key == "desired_temperature")
    {
      setDesiredTemperature(device, mode, value);
    }
    else if (key == "mode" && !root.containsKey("temperature") && !root.containsKey("desired_temperature"))
    {
      // Set just mode and reuse old desired temperature
      setDesiredTemperature(device, mode, device->desired_temperature);
    }
    else if (key == "associate")
    {
      associate(device, value);
    }
    else if (key == "display_actual_temperature")
    {
      displayActualTemperature(device, value);
    }
  }
}

void callback(String topic, byte *payload, unsigned int length)
{
  Debug.println("Handling MQTT message...");
  topic.toLowerCase();
  if (topic == "max/rename")
  {
    rename(payload);
  }
  else if (topic == "max/format")
  {
    format();
  }
  else if (topic == "max/reset")
  {
    ESP.reset();
  }
  else if (topic == "max/set")
  {
    setSelf(payload);
  }
  else if (topic.startsWith("max/") && topic.endsWith("/set"))
  {
    String name = topic.substring(4, topic.length() - 4);
    state *device = findDeviceByName(name);
    if (device)
    {
      set(device, payload);
    }
  }
}

void subscribeToDeviceSetTopics()
{
  state *device;
  for (int i; i < states.size(); i++)
  {
    device = &states[i];
    if (device->name != "")
    {
      String topic = "max/";
      topic += device->name;
      topic += "/set";
      Serial.printf("Subscribing to: %s\n", topic.c_str());
      client.subscribe(topic.c_str(), 1);
    }
  }
}

unsigned long lastCommand = 0;

void rfinit()
{
  lastCommand = millis();
  rf.init();
  Serial.println("Init receive!");
  rf.initReceive();
  Serial.println("Done");
}

void ICACHE_RAM_ATTR messageReceivedInterrupt()
{
  checkForNewPacket();
}

void setup(void)
{
  Serial.begin(115200);

  setup_wifi();
  ArduinoOTA.begin();

#ifdef BURNER_RELAY_PIN
  pinMode(BURNER_RELAY_PIN, OUTPUT);
#endif
  stopBurner();

  loadConfig();

  Serial.println("Setting up time...");
  setupTime();
  Serial.println("Finished.");
  bootedAt = String(ntp.formattedTime("%Y-%m-%d %H:%M:%S"));

  client.setServer(MQTT_SERVER, 1883);
  client.setCallback(callback);

  Serial.println("RF Init");
  rfinit();
  Serial.println("RF Init done");
  pinMode(CC1101_IRQ_PIN, INPUT);
  attachInterrupt(CC1101_IRQ_PIN, messageReceivedInterrupt, RISING);
}

#define RUN 1
#define KEEP 0
#define STOP -1

int last_heating_state = STOP;

bool sendCurrentTimeTo(byte *address, byte msgcnt = 0, byte group = 0, bool longPreamble = true)
{
  if (!isTimeSynced()) {
    Debug.println("Time not synced, cannot send.");
    return false;
  }

  CC1101Packet outMessage;
  outMessage.data[0] = 0x0F; // Length
  outMessage.data[1] = msgcnt;
  outMessage.data[2] = 0x04; //msgflag
  outMessage.data[3] = TIME_INFORMATION_CMD;
  outMessage.data[4] = myAddress[0];
  outMessage.data[5] = myAddress[1];
  outMessage.data[6] = myAddress[2];
  outMessage.data[7] = address[0];
  outMessage.data[8] = address[1];
  outMessage.data[9] = address[2];
  outMessage.data[10] = group; // GroupId

  outMessage.data[11] = ntp.year() - 2000;
  outMessage.data[12] = ntp.day();
  outMessage.data[13] = ntp.hours();
  outMessage.data[14] = ntp.minutes() | ((ntp.month() & 0x0C) << 4);
  outMessage.data[15] = ntp.seconds() | ((ntp.month() & 0x03) << 6);

  outMessage.length = 16;

  Debug.print("Sending time: ");
  printTime();
  Debug.println();

  addToQueue(outMessage, longPreamble);
  return true;
}

unsigned long last_time_sync_to_devices = millis();
byte time_sync_device_chunk = 0;

#define TIME_SYNC_CHUNKS 6
void syncTimeToDevices()
{
  last_time_sync_to_devices = millis();
  state *device;
  Debug.printf("Syncing time to chunk number %i\n", time_sync_device_chunk);
  for (int i; i < states.size(); i++)
  {
    device = &states[i];
    if (i % TIME_SYNC_CHUNKS == time_sync_device_chunk && (device->type == DEVICE_HEATING_THERMOSTAT || device->type == DEVICE_WALL_THERMOSTAT))
    {
      sendCurrentTimeTo(device->address, msgCounter++);
    }
  }

  if (++time_sync_device_chunk >= TIME_SYNC_CHUNKS)
  {
    time_sync_device_chunk = 0;
  }
}

void loop(void)
{
  wifiMulti.run();
  static unsigned long last_credited_at = millis();
  ArduinoOTA.handle();
  yield();
  ntp.update();

  if (millis() > 10 * 24 * 60 * 60 * 1000) {
    Debug.println("******** Restarting after 10 days!");
    ESP.restart();
  }

  if (!received_messages.empty())
  {
    CC1101Packet message = received_messages.front();
    handle(&message);
    received_messages.pop();
  }

  sendMessageFromQueue();
  yield();
  mqttLoop();
  yield();

#ifdef CREDIT_15MIN
  if (millis() - last_credited_at > 15 * 60 * 1000)
  {
    creditMs = CREDIT_15MIN;
    last_credited_at = millis();
  }
#endif

  if (config_changed && millis() - last_config_changed > 60 * 1000)
  {
    config_changed = false;
    last_config_changed = millis(); // Timeout after config change
    saveConfig();
  }

  // Sync time to device chunk every hour
  if (millis() - last_time_sync_to_devices > 60 * 60 * 1000)
  {
    syncTimeToDevices();
    yield();
  }

  if (!published_started_at_state)
  {
    publishState();
  }

  syncValvesToWallThermostats();

  const int heating_needed = isHeatingNeeded();
  if (heating_needed == RUN)
  {
    startBurner();

    if (last_heating_state != RUN)
    {
      Debug.println("******** Furnace run!");
      furnace_running = true;
      publishState();
    }
  }
  else if (heating_needed == STOP)
  {
    stopBurner();

    if (last_heating_state != STOP)
    {
      Debug.println("******** Furnace stop!");
      furnace_running = false;
      publishState();
    }
  }
  last_heating_state = heating_needed;

  // We're not receiving commands, try to rfinit.
  if (millis() - lastCommand >= 5 * 60 * 1000)
  {
    rfinit();
    Debug.println("We're not getting messages, trying to reinit.");
    client.publish("max/init", "RF init");
  }
}

void bytesToString(char *buffer, byte *data, int length)
{
  for (int i = 0; i < length; i++)
  {
    sprintf(&buffer[i * 2], "%02X", data[i]);
  }
  buffer[length * 2] = '\0';
}

void checkForNewPacket()
{
  CC1101Packet message;

  if (rf.receiveData(&message))
  {
    received_messages.push(message);
  }
}

byte getByte(CC1101Packet *packet, unsigned int position)
{
  if (position > packet->length)
  {
    return 0x00;
  }
  else
  {
    return packet->data[position];
  }
}

template <size_t N>
void getBytes(byte (&arr)[N], CC1101Packet *packet, unsigned int position)
{
  for (int i = 0; i < N; i++)
  {
    arr[i] = getByte(packet, position + i);
  }
}

#define bitCopy(from, to, bit) (bitWrite(to, bit, bitRead(from, bit)))

String modeToString(int mode) {
  switch (mode) {
    case MODE_AUTO:
      return "auto";
    case MODE_MANUAL:
      return "manual";
    case MODE_TEMPORARY:
      return "temporary";
    case MODE_BOOST:
      return "boost";
    default:
      return "unknown";
  }
}

int stringToMode(String mode)
{
  if (mode == "auto")
  {
    return MODE_AUTO;
  }
  else if (mode == "temporary")
  {
    return MODE_TEMPORARY;
  }
  else if (mode == "boost")
  {
    return MODE_BOOST;
  }
  else
  {
    return MODE_MANUAL;
  }
}

String typeToString(int type)
{
  switch (type)
  {
  case DEVICE_CUBE:
    return "cube";
  case DEVICE_HEATING_THERMOSTAT:
    return "heater";
  case DEVICE_WALL_THERMOSTAT:
    return "thermostat";
  case DEVICE_SHUTTER_CONTACT:
    return "shutter_contact";
  default:
    return "unknown";
  }
}

void setType(state *device, int type)
{
  if (type != device->type)
  {
    Debug.printf("Device type %i is different from a new type %i\n", device->type, type);
    device->type = type;
    config_changed = true;
  }
}

void setMode(state *device, int mode)
{
  if (mode != device->mode)
  {
    device->mode_changed_timestamp = millis();
  }

  device->mode = mode;
  device->mode_timestamp = millis();
}

bool compareAddress(byte *first, const byte *second)
{
  return first[0] == second[0] && first[1] == second[1] && first[2] == second[2];
}

#define STALE_DURATION 10 * 60 * 1000
bool isFresh(unsigned long last_time)
{
  return (millis() - last_time) <= STALE_DURATION;
}

void sendAckTo(byte *address, byte msgcnt = 0)
{
  CC1101Packet outMessage;
  outMessage.data[0] = 11; // Length
  outMessage.data[1] = msgcnt;
  outMessage.data[2] = 0; //msgflag
  outMessage.data[3] = ACK_CMD;

  for (int i = 0; i < 3; ++i) /* src = enc_dst*/
    outMessage.data[4 + i] = myAddress[i];
  for (int i = 0; i < 3; ++i) /* dst = enc_src */
    outMessage.data[7 + i] = address[i];

  outMessage.data[10] = 0; // GroupId
  outMessage.data[11] = ACK_OK;
  outMessage.length = 12;

  Debug.println("Responding with ACK");

  addToQueue(outMessage, false);
}

void parseDateTime(CC1101Packet *packet, short offset)
{
  byte until[3];
  getBytes(until, packet, offset);

  short day = until[0] & 0x1F;
  short month = ((until[0] & 0xE0) >> 4) | (until[1] >> 7);
  short year = until[1] & 0x3F + 2000;
  short minutes = 0;
  short minute30Chunks = until[2] & 0x3F;

  if (minute30Chunks % 2)
  {
    minutes = 30;
  }

  short hours = minute30Chunks / 2;

  Serial.printf("%i.%i.%i %i:%i", day, month, year, hours, minutes);
}

state *findWallThermostatByRoom(String room)
{
  if (room == "")
  {
    return 0;
  }

  state *device;
  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];

    if (device->type == DEVICE_WALL_THERMOSTAT && device->room == room)
    {
      return device;
    }
  }

  return 0;
}

void syncValvesToWallThermostats()
{
  state *device;
  // Clear valve position info
  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];

    if (device->type == DEVICE_WALL_THERMOSTAT)
    {
      device->valve_position = UNDEFINED;
    }
  }

  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];

    if (device->type == DEVICE_HEATING_THERMOSTAT)
    {
      state *thermostat = findWallThermostatByRoom(device->room);

      if (thermostat && isFresh(device->valve_timestamp) && device->valve_position > thermostat->valve_position)
      {
        thermostat->valve_position = device->valve_position;
        thermostat->valve_timestamp = device->valve_timestamp;
      }
    }
  }
}

const byte MINIMUM_VALVE_POSITION_TO_HEAT PROGMEM = 53;
const float HEAT_END_OFFSET PROGMEM = -0.2;
const float HEAT_START_OFFSET PROGMEM = -0.4;

int compareTemperature(float measured_temperature, float desired_temperature)
{
  if (measured_temperature == UNDEFINED || desired_temperature == UNDEFINED)
  {
    return STOP;
  }

  if (measured_temperature <= desired_temperature + HEAT_START_OFFSET)
  {
    return RUN;
  }

  if (measured_temperature >= desired_temperature + HEAT_END_OFFSET)
  {
    return STOP;
  }

  return KEEP;
}

#define TWO_MINUTES 2 * 60 * 1000

unsigned long boost_duration_chunks_to_ms(byte boost_duration)
{
  byte minutes = boost_duration * 5;

  if (boost_duration == 7)
  {
    minutes = 60;
  }

  return minutes * 60 * 1000;
}

int isHeatingNeeded()
{
  // -1 Stop
  //  0 Keep
  //  1 Run

  int result = STOP;

  state *device;
  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];

    if (!isFresh(device->timestamp) || device->mode == UNDEFINED)
    {
      continue;
    }

    if (device->type == DEVICE_HEATING_THERMOSTAT)
    {
      // Ignore devices with binded thermostat
      state *thermostat = findWallThermostatByRoom(device->room);
      if (thermostat)
      {
        continue;
      }
    }

    if (device->mode == MODE_BOOST && millis() - device->mode_changed_timestamp < boost_duration_chunks_to_ms(device->boost_duration) - TWO_MINUTES)
    {
      return RUN;
    }

    if (!isFresh(device->desired_temperature_timestamp) || !isFresh(device->measured_temperature_timestamp))
    {
      continue;
    }

    // Manual mode on radiator thermostat if wall thermostat not binded
    if (
        (device->type == DEVICE_WALL_THERMOSTAT || (device->type == DEVICE_HEATING_THERMOSTAT && device->mode == MODE_MANUAL)) &&
        device->valve_position > MINIMUM_VALVE_POSITION_TO_HEAT)
    {
      result = max(result, compareTemperature(device->measured_temperature, device->desired_temperature));
    }
  }

  return result;
}

void sendConfigurationTo(state *device)
{
  Debug.printf("Restoring configuration for %s after factory reset.\n", device->name.c_str());

  sendCurrentTimeTo(device->address, msgCounter++, device->group, true);

  // Restore display actual temperature
  if (device->display_actual_temperature != UNDEFINED)
  {
    displayActualTemperature(device, device->display_actual_temperature);
  }

  configValveFunctions(device, device->decalc_weekday, device->decalc_hour, device->boost_duration, device->boost_valve_position, device->max_valve_setting, device->valve_offset);
  setTemperatureSettings(device, device->comfort_temperature, device->eco_temperature, device->max_temperature, device->min_temperature, device->window_open_temperature);

  // Restore associations
  for (byte pos = 0; pos < device->associated_devices.size(); pos += 3)
  {
    state *toDevice = findDeviceByAddress(device->associated_devices.data() + pos);

    if (toDevice)
    {
      sendAssociateBetween(device, toDevice);
    }
  }

  // Restore schedule
  for (byte weekDay = 0; weekDay < 7; weekDay++)
  {
    if (device->schedule_size[weekDay] > 0)
    {
      sendScheduleTo(device, weekDay, device->schedule[weekDay], device->schedule_size[weekDay]);
    }
  }
}

void handle(CC1101Packet *packet)
{
  char buffer[packet->length * 2 + 1];

  bytesToString(buffer, packet->data, packet->length);
  Debug.println("-----------------------------");
  Debug.printf("Received message, %s, length: ", buffer);
  Debug.print(packet->length, DEC);
  Debug.print(", ");

  bool crcOK = packet->data[0] == packet->length - 3;
  if (!crcOK)
  {
    Debug.println("CRC NOT OK");
    return;
  }

  int rssi = getByte(packet, packet->length - 2);

  if (rssi >= 128)
  {
    rssi = (rssi - 256) / 2 - 74;
  }
  else
  {
    rssi = rssi / 2 - 74;
  }

  Debug.printf("CRC OK, RSSI %i\n", rssi);
  // client.publish("max/raw", buffer);

  lastCommand = millis();

  byte msgcnt = getByte(packet, 1);
  byte flags = getByte(packet, 2); // 04 to group or 00 to specific device
  byte command = getByte(packet, 3);
  byte src[3];
  getBytes(src, packet, 4);
  byte dst[3];
  getBytes(dst, packet, 7);
  byte group = getByte(packet, 10);

  const bool isToMyself = compareAddress(dst, myAddress);

  state *device = findDeviceByAddress(src);
  if (!device)
  {
    if (autocreate || pairing_enabled)
    {
      states.push_back(state());
      device = &states.back();
    }
    else
    {
      // Ignore device
      return;
    }
  }

  device->address[0] = src[0];
  device->address[1] = src[1];
  device->address[2] = src[2];

  char address[7];
  char dstAddress[7];
  bytesToString(address, device->address, 3);
  bytesToString(dstAddress, dst, 3);

  if (device->name == "")
  {
    device->name = address;
    config_changed = true;
  }

  Debug.printf("Message from: %s (%s) to %s (group %i), msgcnt: %i, command: %i\n", device->name.c_str(), address, dstAddress, group, msgcnt, command);

  device->timestamp = millis();

  switch (command)
  {
  case TIME_INFORMATION_CMD:
  {
    Debug.printf("Time information, isToMyself %i\n", isToMyself);

    // Parse it and correct it if wrong?
    if (isToMyself && packet->length == 13)
    {
      sendCurrentTimeTo(src, msgcnt, group, false);
    }

    break;
  }
  case THERMOSTAT_STATE_CMD:
  {
    setType(device, DEVICE_HEATING_THERMOSTAT);
    short valve_position = getByte(packet, 12);
    device->valve_position = valve_position;
    device->valve_timestamp = millis();
  }
  case WALL_THERMOSTAT_STATE_CMD:
  {
    if (device->type == UNDEFINED)
    {
      setType(device, DEVICE_WALL_THERMOSTAT);
    }
    byte flags = getByte(packet, 11);
    // data.dstsetting = getBits(bits2, 3, 1); //is automatically switching to DST activated
    // data.langateway = getBits(bits2, 4, 1); //??
    // data.panel = getBits(bits2, 5, 1); //1 if the heating thermostat is locked for manually setting the temperature at the device
    short mode = 0;
    bitCopy(flags, mode, 0);
    bitCopy(flags, mode, 1);
    setMode(device, mode);

    bool rferror = bitRead(flags, 6);
    device->rf_error = rferror;
    bool battery_low = bitRead(flags, 7);
    device->low_battery = battery_low;

    if (packet->length == 17)
    {
      parseDateTime(packet, 14);
    }

    byte displayActualTemperature = getByte(packet, 12);
    byte desiredTemperatureRaw = getByte(packet, 13);
    float desiredTemperature = (desiredTemperatureRaw & 0x7F) / 2.0;
    float measuredTemperature = (((getByte(packet, 14) & 0x01) << 8) + getByte(packet, 15)) / 10.0;
    device->desired_temperature = desiredTemperature;
    device->desired_temperature_timestamp = millis();
    device->measured_temperature = measuredTemperature;
    device->measured_temperature_timestamp = millis();
    Debug.print("STATE/Desired Temperature: ");
    Debug.print(desiredTemperature, 1);
    Debug.print(" Measured temperature: ");
    Debug.println(measuredTemperature, 1);
    // TODO: implement until?
    break;
  }
  case ACK_CMD:
  {
    byte payload = getByte(packet, 11);
    byte flags = getByte(packet, 12);
    // data.dstsetting = getBits(bits2, 3, 1); //is automatically switching to DST activated
    // data.langateway = getBits(bits2, 4, 1); //??
    // data.panel = getBits(bits2, 5, 1); //1 if the heating thermostat is locked for manually setting the temperature at the device
    short mode = 0;
    bitCopy(flags, mode, 0);
    bitCopy(flags, mode, 1);
    setMode(device, mode);

    bool rferror = bitRead(flags, 6);
    device->rf_error = rferror;
    bool battery_low = bitRead(flags, 7);
    device->low_battery = battery_low;

    Debug.print("ACK ");
    if (payload == ACK_OK)
    {
      Debug.print("OK");
    }
    else
    {
      Debug.print("Invalid ");
      Debug.print(payload, 10);
    }
    Debug.printf(", type: %i, mode: %i", device->type, device->mode);

    if (isToMyself)
    {
      Debug.print(", is to myself");
      ackMessageInQueue(msgcnt);
    }

    if (device->type == DEVICE_HEATING_THERMOSTAT)
    {
      short valve_position = getByte(packet, 13);
      Debug.printf(", valve_position: %i", valve_position);
      device->valve_position = valve_position;
      device->valve_timestamp = millis();

      byte desiredTemperatureRaw = getByte(packet, 14);
      float desiredTemperature = (desiredTemperatureRaw & 0x7F) / 2.0;
      device->desired_temperature = desiredTemperature;
      device->desired_temperature_timestamp = millis();
      Debug.printf(", desired temperature: ");
      Debug.print(desiredTemperature);
    }
    else if (device->type == DEVICE_WALL_THERMOSTAT)
    {
      byte displayActualTemperature = getByte(packet, 13);
      setDisplayActualTemperatureState(device, displayActualTemperature);
      byte desiredTemperatureRaw = getByte(packet, 14);
      float desiredTemperature = (desiredTemperatureRaw & 0x7F) / 2.0;
      device->desired_temperature = desiredTemperature;
      device->desired_temperature_timestamp = millis();

      if (displayActualTemperature == DISPLAY_CURRENT_SETPOINT)
      {
        Debug.print(", display current setpoint");
      }
      else
      {
        Debug.print(", display actual temperature");
      }
      Debug.printf(", desired temperature: ");
      Debug.print(desiredTemperature);
    }
    Debug.println();
    break;
  }
  case WALL_THERMOSTAT_CONTROL_CMD:
  {
    setType(device, DEVICE_WALL_THERMOSTAT);

    float desiredTemperature = (getByte(packet, 11) & 0x7F) / 2.0;
    float measuredTemperature = (((getByte(packet, 11) & 0x80) << 1) + getByte(packet, 12)) / 10.0;
    device->desired_temperature = desiredTemperature;
    device->desired_temperature_timestamp = millis();
    device->measured_temperature = measuredTemperature;
    device->measured_temperature_timestamp = millis();
    Serial.print("Control/Desired Temperature:  ");
    Serial.print(desiredTemperature);
    Serial.print(" Measured temperature: ");
    Serial.println(measuredTemperature);
    break;
  }
  case SHUTTER_CONTACT_STATE_CMD:
  {
    setType(device, DEVICE_SHUTTER_CONTACT);
    byte flags = getByte(packet, 11);

    bool isOpen = bitRead(flags, 1);
    device->is_open = isOpen;

    bool rferror = bitRead(flags, 6);
    device->rf_error = rferror;
    bool battery_low = bitRead(flags, 7);
    device->low_battery = battery_low;

    Debug.printf("Shutter contact state, isopen: %i\n", isOpen);

    if (isToMyself)
    {
      sendAckTo(src, msgcnt);
    }
    break;
  }
  case SET_TEMPERATURE_CMD:
  {
    // Sent only by DEVICE_WALL_THERMOSTAT & DEVICE_CUBE
    byte modeAndTemperature = getByte(packet, 11);

    short mode = modeAndTemperature >> 6;
    setMode(device, mode);

    float desiredTemperature = (modeAndTemperature & 0x3F) / 2.0;
    device->desired_temperature = desiredTemperature;
    device->desired_temperature_timestamp = millis();

    // Date until, only in case of vacation mode
    // parseDateTime(packet, 14);

    Debug.printf("Set temperature, mode %i, desired_temperature: ", mode);
    Debug.println(desiredTemperature);
    if (isToMyself)
    {
      sendAckTo(src, msgcnt);
    }
    break;
  }
  case PUSH_BUTTON_STATE_CMD:
  {
    // Not implemented
    if (isToMyself)
    {
      sendAckTo(src, msgcnt);
    }
    break;
  }
  case PAIR_PING_CMD:
  {
    Debug.print("Pair ping request");
    if (isToMyself || pairing_enabled)
    {
      Debug.print(", is to myself");
      CC1101Packet outMessage;
      outMessage.data[0] = 11; // Length
      outMessage.data[1] = msgcnt;
      outMessage.data[2] = 0x0; //msgflag
      outMessage.data[3] = PAIR_PONG_CMD;
      outMessage.data[4] = myAddress[0];
      outMessage.data[5] = myAddress[1];
      outMessage.data[6] = myAddress[2];
      outMessage.data[7] = src[0];
      outMessage.data[8] = src[1];
      outMessage.data[9] = src[2];
      outMessage.data[10] = group; // GroupId
      outMessage.data[11] = 0x00;  //Payload
      outMessage.length = 12;
      Debug.println(", responding with PairPong.");
      addToQueue(outMessage, false);

      // Paring of a new device or after factory reset
      if (!isToMyself)
      {
        sendConfigurationTo(device);
      }
    }
    else
    {
      Debug.println();
    }
  }
  }

  syncValvesToWallThermostats();

  char output[256];

  StaticJsonDocument<capacity> doc;

  if (device->type != UNDEFINED)
  {
    doc["type"] = typeToString(device->type);
  }
  if (device->room != "")
  {
    doc["room"] = device->room;
  }
  if (device->mode != UNDEFINED)
  {
    doc["mode"] = modeToString(device->mode);
  }
  if (device->measured_temperature != UNDEFINED)
  {
    doc["measured_temperature"] = device->measured_temperature;
  }

  if (device->desired_temperature != UNDEFINED)
  {
    doc["desired_temperature"] = device->desired_temperature;
  }

  if (device->valve_position != UNDEFINED)
  {
    doc["valve_position"] = device->valve_position;
  }

  if (device->is_open != UNDEFINED)
  {
    doc["open"] = (bool)device->is_open;
  }

  if (device->low_battery != UNDEFINED)
  {
    doc["low_battery"] = (bool)device->low_battery;
  }

  if (device->rf_error != UNDEFINED)
  {
    doc["rf_error"] = (bool)device->rf_error;
  }
  doc["rssi"] = rssi;
  serializeJson(doc, output);
  String topic = "max/";
  topic += device->name;
  client.publish(topic.c_str(), output, true);

  Debug.println(output);
}
