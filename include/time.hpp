#include <NTP.h>

extern NTP ntp;

void setupTime();
void printTime();

#define TENYEARS 315360000UL
bool isTimeSynced();