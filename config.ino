#include "FS.h"
#include "max.h"
#include <ArduinoJson.h>
#include "state.h"
#include "max.h"

const size_t CONFIG_CAPACITY PROGMEM = JSON_OBJECT_SIZE(10) + 1024;

void format()
{
  Debug.println("Formatting...");
  SPIFFS.format();
  Debug.println("Done...");
}

void parseDeviceConfigFile(state *device, String filename)
{
  DynamicJsonDocument config(CONFIG_CAPACITY);
  File file = SPIFFS.open(filename, "r");
  if (!file)
  {
    Debug.printf("Failed to open config file %s\n", filename.c_str());
    return;
  }

  DeserializationError error = deserializeJson(config, file);
  if (error)
  {
    Debug.println("Failed to parse config file");
    Debug.println(error.c_str());
    config.clear();
    file.close();
    return;
  }
  file.close();

  JsonObject root = config.as<JsonObject>();
  Debug.printf("Usage is: %i\n", config.memoryUsage());

  const char *address = root["address"];
  stringToBytes(device->address, address, 6);
  device->name = root["name"] | "";
  device->room = root["room"] | "";
  device->type = root["type"] | UNDEFINED;
  device->group = root["group"] | 0;

  if (device->type == DEVICE_WALL_THERMOSTAT || device->type == DEVICE_HEATING_THERMOSTAT)
  {
    if (root.containsKey("display_actual_temperature"))
    {
      device->display_actual_temperature = root["display_actual_temperature"];
    }

    device->eco_temperature = root["eco"] | 17;
    device->comfort_temperature = root["comfort"] | 21;
    device->max_temperature = root["max"] | 30.5;
    device->min_temperature = root["min"] | 4.5;
    device->window_open_temperature = root["window_open"] | 4.5;

    device->decalc_weekday = root["decalc_weekday"] | 0;
    device->decalc_hour = root["decalc_hour"] | 12;
    device->boost_duration = root["boost_duration"] | 30;
    device->boost_valve_position = root["boost_valve_position"] | 100;
    device->max_valve_setting = root["max_valve_setting"] | 100;
    device->valve_offset = root["valve_offset"] | 0;

    byte association_address[3];
    if (root.containsKey("associations"))
    {
      JsonArray associations = root["associations"].as<JsonArray>();
      for (JsonVariant v : associations)
      {
        const char *value = v.as<const char *>();
        stringToBytes(association_address, value, 6);
        device->associated_devices.insert(device->associated_devices.end(), association_address, association_address + 3);
      }
    }

    if (root.containsKey("schedule"))
    {
      JsonArray schedule = root["schedule"].as<JsonArray>();
      byte weekDay = 0;
      for (JsonVariant v : schedule)
      {
        if (device->schedule[weekDay] != 0)
        {
          delete[] device->schedule[weekDay];
        }

        if (!v.isNull())
        {
          const char *value = v.as<const char *>();
          byte schedule_size = strlen(value);
          device->schedule_size[weekDay] = schedule_size / 2;
          device->schedule[weekDay] = new byte[schedule_size / 2];
          stringToBytes(device->schedule[weekDay], value, schedule_size);
        }

        weekDay++;
      }
    }
  }

  Debug.printf("Loaded name: %s, type: %i\n", device->name.c_str(), device->type);
  config.clear();
}

bool loadConfig()
{
  if (!SPIFFS.begin())
  {
    Debug.println("Failed to mount file system");
    return false;
  }

  DynamicJsonDocument config(CONFIG_CAPACITY);
  File file = SPIFFS.open("/config.json", "r");
  if (!file)
  {
    Debug.println("Failed to open main config file");
    return false;
  }

  DeserializationError error = deserializeJson(config, file);
  if (error)
  {
    Debug.println("Failed to parse main config file");
    Debug.println(error.c_str());
  }

  file.close();

  const char *myAddressConfig = config["address"] | "123456";
  stringToBytes(myAddress, myAddressConfig, 6);
  autocreate = config["autocreate"] | true;

  int i = 0;
  Dir dir = SPIFFS.openDir("/devices");
  state *device;
  while (dir.next())
  {
    String path = dir.fileName();
    Serial.printf("Parsing config at: %s\n", path.c_str());

    states.push_back(state());
    device = &states.back();
    parseDeviceConfigFile(device, path);
  }

  return true;
}

char schedule_buffer[DAY_SCHEDULE_LENGTH];

bool saveConfig()
{
  File configFile = SPIFFS.open("/config.json", "w");
  if (!configFile)
  {
    Debug.println("Failed to open config file for writing");
    return false;
  }

  DynamicJsonDocument config(CONFIG_CAPACITY);
  char address[7];
  bytesToString(address, myAddress, 3);
  config["address"] = address;
  config["autocreate"] = autocreate;

  Debug.println("Saving main config file...");
  if (serializeJson(config, configFile) == 0)
  {
    Debug.println("Failed to write to file");
  }
  Debug.println("Serialized");

  yield();
  configFile.close();

  state *device;
  for (int i = 0; i < states.size(); i++)
  {
    device = &states[i];
    if (device->address[0] == 0 &&
        device->address[1] == 0 &&
        device->address[2] == 0)
    {
      break;
    }
    config.clear();

    bytesToString(address, device->address, 3);

    String path = "/devices/";
    path += address;
    path += ".json";

    File configFile = SPIFFS.open(path, "w");

    config["name"] = device->name;
    config["room"] = device->room;
    config["address"] = address;
    config["type"] = device->type;
    config["group"] = device->group;

    JsonArray associations = config.createNestedArray("associations");
    for (byte pos = 0; pos < device->associated_devices.size(); pos += 3)
    {
      bytesToString(address, device->associated_devices.data() + pos, 3);
      associations.add(address);
    }

    if (device->type == DEVICE_WALL_THERMOSTAT || device->type == DEVICE_HEATING_THERMOSTAT)
    {
      if (device->display_actual_temperature != UNDEFINED)
      {
        config["display_actual_temperature"] = (bool)device->display_actual_temperature;
      }

      config["eco"] = device->eco_temperature;
      config["comfort"] = device->comfort_temperature;
      config["max"] = device->max_temperature;
      config["min"] = device->min_temperature;
      config["window_open"] = device->window_open_temperature;

      config["decalc_weekday"] = device->decalc_weekday;
      config["decalc_hour"] = device->decalc_hour;
      config["boost_duration"] = device->boost_duration;
      config["boost_valve_position"] = device->boost_valve_position;
      config["max_valve_setting"] = device->max_valve_setting;
      config["valve_offset"] = device->valve_offset;

      JsonArray schedule = config.createNestedArray("schedule");
      for (byte weekDay = 0; weekDay < 7; weekDay++)
      {
        if (device->schedule_size[weekDay] > 0)
        {
          bytesToString(schedule_buffer, device->schedule[weekDay], device->schedule_size[weekDay]);
          schedule.add(schedule_buffer);
        }
        else
        {
          schedule.add(serialized("null"));
        }
      }
    }

    Debug.printf("Saving device config file to: %s...\n", path.c_str());
    serializeJsonPretty(config, Debug);
    Debug.println();
    Debug.printf("Usage is: %i\n", config.memoryUsage());
    if (serializeJson(config, configFile) == 0)
    {
      Debug.println("Failed.");
    }
    else
    {
      Debug.println("Done.");
    }

    yield();
    configFile.close();
  }
}