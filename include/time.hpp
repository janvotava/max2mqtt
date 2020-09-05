#include <TimeLib.h>

void setupTime();
void sPrintI00(int val);
void sPrintDigits(int val);
void printTime(time_t t);
unsigned long sendNTPpacket();

time_t toLocal(time_t utc);
time_t getBootTime();
time_t getTime();

bool syncTime();