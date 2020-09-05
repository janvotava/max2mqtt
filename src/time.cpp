#include "Arduino.h"
#include <WiFiUdp.h>
#include <NTP.h>
#include <time.hpp>

WiFiUDP wifiUdp;
NTP ntp(wifiUdp);

void setupTime() {
  // Central European Time
  ntp.ruleDST("CEST", Last, Sun, Mar, 2, 120); // last sunday in march 2:00, timetone +120min (+1 GMT + 1h summertime offset)
  ntp.ruleSTD("CET", Last, Sun, Oct, 3, 60); // last sunday in october 3:00, timezone +60min (+1 GMT)

  ntp.begin();
}

void printTime() {
  Serial.println(ntp.formattedTime("%Y-%m-%d %H:%M:%S"));
}

bool isTimeSynced() {
  return ntp.epoch() > TENYEARS;
}