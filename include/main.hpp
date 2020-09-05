#include "Arduino.h"
#include "MaxCC1101.h"
#include "CC1101Packet.h"
#include "state.h"
#include <ArduinoJson.h>
#include <vector>
#include <ESP8266WiFi.h>
#include <PubSubClient.h>

void startBurner();
void stopBurner();
void setup_wifi();
void send(CC1101Packet *packet, bool preamble);
void sendMessageFromQueue();
void ackMessageInQueue(byte msgcnt);
void addToQueue(CC1101Packet packet, bool longPreamble, bool waitForAck);
void rename(byte *payload);
void setRoom(state *device, String room);
void setGroup(state *device, byte group);
void setDesiredTemperature(state *device, int mode, float temperature);
void sendScheduleTo(state *device, byte weekDay, byte *schedule, byte size);
void setSchedule(state *device, String day, JsonObject schedule_config);
void addAssociation(state *device, byte *address);
void addLinkPartner(byte *address, byte *to, byte type);
void sendAssociateBetween(state *device, state *toDevice);
void associate(state *device, String to);
void setTemperatureSettings(state *device, float comfort, float eco, float max, float min, float window_open);
void setDisplayActualTemperatureState(state *device, bool display_actual_temperature);
void configValveFunctions(state *device, byte decalc_weekday, byte decalc_hour, byte boost_duration, byte boost_valve_position, byte max_valve_setting, byte valve_offset);
void displayActualTemperature(state *device, bool isEnabled);
void setAddress(const char *address);
void setSelf(byte *payload);
void publishState();
void set(state *device, byte *payload);
void callback(String topic, byte *payload, unsigned int length);
void subscribeToDeviceSetTopics();
void rfinit();
void ICACHE_RAM_ATTR messageReceivedInterrupt();
void setup(void);
void syncTimeToDevices();
void loop(void);
void bytesToString(char *buffer, byte *data, int length);
void checkForNewPacket();

template <size_t N>
void getBytes(byte (&arr)[N], CC1101Packet *packet, unsigned int position);

void setType(state *device, int type);
void setMode(state *device, int mode);
void sendAckTo(byte *address, byte msgcnt);
void parseDateTime(CC1101Packet *packet, short offset);
void syncValvesToWallThermostats();
void sendConfigurationTo(state *device);
void handle(CC1101Packet *packet);

int stringToMode(String mode);
unsigned int stringToBytes(byte *data, const char *payload, unsigned int length);

int isHeatingNeeded();

extern bool autocreate;
extern std::vector<state> states;
extern byte myAddress[3];

extern WiFiClient espClient;
extern PubSubClient client;

#if TELNET_DEBUGGER
RemoteDebug Debug;
#else
#define Debug Serial
#endif