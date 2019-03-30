#include <ESP8266WiFi.h>
#include <WiFiUdp.h>
#include "src/lib/Time/TimeLib.h"
#include "src/lib/Timezone/src/Timezone.h"

unsigned int localUdpPort = 2390; // local port to listen for UDP packets

const char *ntpServers[] = {"0.europe.pool.ntp.org", "1.europe.pool.ntp.org", "2.europe.pool.ntp.org", "3.europe.pool.ntp.org"};
const int ntpServersLength = (sizeof(ntpServers) / sizeof(ntpServers[0]));

#define NTP_PACKET_SIZE 48 // NTP time stamp is in the first 48 bytes of the message

byte packetBuffer[NTP_PACKET_SIZE]; //buffer to hold incoming and outgoing packets

// A UDP instance to let us send and receive packets over UDP
WiFiUDP udp;

//Central European Time (Frankfurt, Paris)
TimeChangeRule CEST = {"CEST", Last, Sun, Mar, 2, 120}; //Central European Summer Time
TimeChangeRule CET = {"CET", Last, Sun, Oct, 3, 60};    //Central European Standard Time
Timezone CE(CEST, CET);

void setupTime()
{
  udp.begin(localUdpPort);
}

unsigned long _lastUpdate;
unsigned long epoch = 0;

bool syncTime()
{
  // Time synced less than 15minutes ago, do not sync.
  if (epoch && millis() - _lastUpdate < 15 * 60 * 1000)
  {
    return true;
  }

  sendNTPpacket();

  byte timeout = 0;
  int cb = 0;
  do
  {
    delay(10);
    cb = udp.parsePacket();
    if (timeout > 200)
    {
      Serial.println("Fail.");
      return false; // timeout after 2000 ms
    }
    timeout++;
  } while (cb == 0);

  udp.read(packetBuffer, NTP_PACKET_SIZE); // read the packet into the buffer
  Serial.println("Done.");

  unsigned long highWord = word(packetBuffer[40], packetBuffer[41]);
  unsigned long lowWord = word(packetBuffer[42], packetBuffer[43]);
  unsigned long secsSince1900 = highWord << 16 | lowWord;
  const unsigned long seventyYears = 2208988800UL;
  epoch = secsSince1900 - seventyYears + (10 * timeout) / 1000;

  _lastUpdate = millis();
}

time_t toLocal(time_t utc) {
  TimeChangeRule *tcr;
  return CE.toLocal(utc, &tcr);
}

time_t getBootTime()
{
  if (!epoch)
  {
    return 0;
  }

  time_t utc = epoch - millis() / 1000;

  return toLocal(utc);
}

time_t getTime()
{
  if (!epoch)
  {
    return 0;
  }

  time_t utc = epoch + (millis() - _lastUpdate) / 1000;

  return toLocal(utc);
}

// send an NTP request to the time server at the given address
unsigned long sendNTPpacket()
{
  IPAddress timeServerIP; // time.nist.gov NTP server address
  const char *ntpServerName = ntpServers[random(0, ntpServersLength)];

  WiFi.hostByName(ntpServerName, timeServerIP);
  Serial.print("Sending NTP packet... ");
  // set all bytes in the buffer to 0
  memset(packetBuffer, 0, NTP_PACKET_SIZE);
  // Initialize values needed to form NTP request
  // (see URL above for details on the packets)
  packetBuffer[0] = 0b11100011; // LI, Version, Mode
  packetBuffer[1] = 0;          // Stratum, or type of clock
  packetBuffer[2] = 6;          // Polling Interval
  packetBuffer[3] = 0xEC;       // Peer Clock Precision
  // 8 bytes of zero for Root Delay & Root Dispersion
  packetBuffer[12] = 49;
  packetBuffer[13] = 0x4E;
  packetBuffer[14] = 49;
  packetBuffer[15] = 52;

  // all NTP fields have been given values, now
  // you can send a packet requesting a timestamp:
  udp.beginPacket(timeServerIP, 123); //NTP requests are to port 123
  udp.write(packetBuffer, NTP_PACKET_SIZE);
  udp.endPacket();
}

//Function to print time with time zone
void printTime(time_t t)
{
  sPrintI00(hour(t));
  sPrintDigits(minute(t));
  sPrintDigits(second(t));
  Serial.print(' ');
  Serial.print(day(t));
  Serial.print('.');
  Serial.print(month(t));
  Serial.print('.');
  Serial.print(year(t));
}

//Print an integer in "00" format (with leading zero).
//Input value assumed to be between 0 and 99.
void sPrintI00(int val)
{
  if (val < 10)
    Serial.print('0');
  Serial.print(val, DEC);
  return;
}

//Print an integer in ":00" format (with leading zero).
//Input value assumed to be between 0 and 99.
void sPrintDigits(int val)
{
  Serial.print(':');
  if (val < 10)
    Serial.print('0');
  Serial.print(val, DEC);
}
