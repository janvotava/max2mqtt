#ifndef STATE_H
#define STATE_H

#include <vector>
#include "max.h"
#define DAY_SCHEDULE_LENGTH 26

typedef struct
{
  String name = "";
  String room = "";
  byte address[3] = {0, 0, 0};
  unsigned long timestamp = millis();
  float desired_temperature = UNDEFINED;
  unsigned long desired_temperature_timestamp = millis();
  float measured_temperature = UNDEFINED;
  unsigned long measured_temperature_timestamp = millis();
  int valve_position = UNDEFINED; // -1 for undefined
  unsigned long valve_timestamp = millis();
  int mode = UNDEFINED; // -1 for undefined
  unsigned long mode_timestamp = millis();
  unsigned long mode_changed_timestamp = millis();
  int type = UNDEFINED;                       // -1 for undefined, TODO: cleanup
  int display_actual_temperature = UNDEFINED; // -1 for undefined
  int is_open = UNDEFINED;                    // -1 for undefined
  int rf_error = UNDEFINED;                   // -1 for undefined
  int low_battery = UNDEFINED;                // -1 for undefined
  byte group = 0;
  float eco_temperature = 17;
  float comfort_temperature = 21;
  float max_temperature = 30.5;
  float min_temperature = 4.5;
  float window_open_temperature = 4.5;

  byte decalc_weekday = 0;
  byte decalc_hour = 12;
  byte boost_duration = 30;
  byte boost_valve_position = 100;
  byte max_valve_setting = 100;
  byte valve_offset = 0;

  byte *schedule[7];
  byte schedule_size[7];

  std::vector<byte> associated_devices;
} state;
#endif
