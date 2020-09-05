#ifndef MAX_H
#define MAX_H

// 2 bytes for RSSI and line quality?
#define MAX_MORITZ_MSG 30 + 2

#define UNDEFINED -1

// "00" : "PairPing",
// "01" : "PairPong",
// "02" : "Ack",
// "03" : "TimeInformation",
// "10" : "ConfigWeekProfile",
// "11" : "ConfigTemperatures", //like eco/comfort etc
// "12" : "ConfigValve",
// "20" : "AddLinkPartner",
// "21" : "RemoveLinkPartner",
// "22" : "SetGroupId",
// "23" : "RemoveGroupId",
// "30" : "ShutterContactState",
// "40" : "SetTemperature", //to thermostat
// "42" : "WallThermostatControl", //by WallMountedThermostat
// "43" : "SetComfortTemperature",
// "44" : "SetEcoTemperature",
// "50" : "PushButtonState",
// "60" : "ThermostatState", //by HeatingThermostat
// "70" : "WallThermostatState",
// "82" : "SetDisplayActualTemperature",
// "F1" : "WakeUp",
// "F0" : "Reset",

#define PAIR_PING_CMD 0x00
#define PAIR_PONG_CMD 0x01
#define ACK_CMD 0x02
#define TIME_INFORMATION_CMD 0x03
#define CONFIG_WEEK_PROFILE_CMD 0x10
#define CONFIG_TEMPERATURES_CMD 0x11
#define CONFIG_VALVE_CMD 0x12
#define ADD_LINK_PARTNER_CMD 0x20
#define REMOVE_LINK_PARTNER_CMD 0x21
#define SET_GROUP_ID_CMD 0x22
#define REMOVE_GROUP_ID_CMD 0x23
#define SHUTTER_CONTACT_STATE_CMD 0x30
#define SET_TEMPERATURE_CMD 0x40
#define WALL_THERMOSTAT_CONTROL_CMD 0x42
#define SET_COMFORT_TEMPERATURE_CMD 0x43
#define SET_ECO_TEMPERATURE_CMD 0x44
#define PUSH_BUTTON_STATE_CMD 0x50
#define THERMOSTAT_STATE_CMD 0x60
#define WALL_THERMOSTAT_STATE_CMD 0x70
#define SET_DISPLAY_ACTUAL_TEMPERATURE_CMD 0x82
#define WAKEUP_CMD 0xF1
#define RESET_CMD 0xF0

#define MODE_AUTO 0x00
#define MODE_MANUAL 0x01
#define MODE_TEMPORARY 0x02
#define MODE_BOOST 0x03

#define DEVICE_CUBE 0x0
#define DEVICE_HEATING_THERMOSTAT 0x1
#define DEVICE_WALL_THERMOSTAT 0x3
#define DEVICE_SHUTTER_CONTACT 0x4

#define ACK_OK 0x01
#define ACK_INVALID 0x81

#define DISPLAY_CURRENT_TEMPERATURE 0x04
#define DISPLAY_CURRENT_SETPOINT 0x00
#endif