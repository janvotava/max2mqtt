// CC1101 radio check: wiring, power pins and crystal frequency offset.
//
// Replaces the firmware (no WiFi, no MQTT, never transmits) and reports over serial at 115200 baud.
// Flash it with `pio run -e cc1101_check -t upload`, see docs/cc1101-shield.md for how to read the output.
#ifdef CC1101_CHECK
#include <Arduino.h>
#include <ESP8266WiFi.h>
#include <SPI.h>
#include "configuration.h"
#include "MaxCC1101.h"

#define KHZ_PER_STEP 1.587 // FREQEST and FSCTRL0 resolution: f_xosc / 2^14 with a 26 MHz crystal
#define MARC_IDLE 0x01
#define MARC_RX 0x0D

static MaxCC1101 rf;

static uint8_t config(uint8_t address) { return rf.readRegister(address, CC1101_CONFIG_REGISTER); }
static uint8_t status(uint8_t address) { return rf.readRegister(address, CC1101_STATUS_REGISTER); }
static uint8_t marcState() { return status(CC1101_MARCSTATE) & CC1101_BITS_MARCSTATE; }
static int toDbm(uint8_t raw) { return (raw >= 128 ? ((int)raw - 256) / 2 : raw / 2) - 74; }

static bool waitForState(uint8_t wanted, uint32_t timeoutMs)
{
  uint32_t start = millis();
  while (millis() - start < timeoutMs)
  {
    if (marcState() == wanted)
      return true;
  }
  return false;
}

static bool result(const char *name, bool ok, const char *hint)
{
  Serial.printf("  %s: %s%s%s\n", name, ok ? "OK" : "FAIL", ok ? "" : " - ", ok ? "" : hint);
  return ok;
}

// Pins 3 MOSI, 4 SCK, 5 MISO, 8 CSN: thousands of register writes read back.
static bool checkSpi()
{
  const uint8_t registers[] = {CC1101_SYNC1, 0x05, 0x06, CC1101_ADDR, 0x0A, 0x0E, 0x0F};
  uint32_t wrong = 0;
  uint8_t value = 0x5A;
  rf.init();
  delay(20);
  for (int i = 0; i < 3000; i++)
  {
    uint8_t address = registers[i % 7];
    value = value * 37 + 11;
    rf.writeRegister(address, value);
    if (config(address) != value)
      wrong++;
    if (i % 500 == 0)
      yield();
  }
  Serial.printf("  3000 register writes read back, %u wrong\n", wrong);
  return result("SPI (pins 3 MOSI, 4 SCK, 5 MISO, 8 CSN)", wrong == 0, "check those four joints");
}

// Pin 6 GDO2: the firmware only receives through the interrupt on this line.
static bool checkIrqLine()
{
  uint32_t wrong = 0;
  pinMode(CC1101_IRQ_PIN, INPUT);
  for (int i = 0; i < 1000; i++)
  {
    rf.writeRegister(CC1101_IOCFG2, (i & 1) ? 0x6F : 0x2F); // GDO2 driven high / low by the chip
    delayMicroseconds(100);
    if (digitalRead(CC1101_IRQ_PIN) != (i & 1))
      wrong++;
    if (i % 250 == 0)
      yield();
  }
  return result("interrupt line (pin 6 GDO2)", wrong == 0, "GDO2 does not reach CC1101_IRQ_PIN");
}

// Pins 1 VCC and 2 GND. A CC1101 with an open supply pin still answers over SPI, because it gets powered
// through its signal pins. Holding all of them low leaves only the real VCC pin, all high only the real GND pin.
static bool checkSupplyPin(bool level)
{
  rf.init();
  rf.initReceive();
  rf.writeCommand(CC1101_SIDLE);
  waitForState(MARC_IDLE, 50);
  rf.writeRegister(CC1101_ADDR, 0x5A);
  rf.writeCommand(CC1101_SRX);
  bool receiving = waitForState(MARC_RX, 100);

  pinMode(SCK, OUTPUT);
  pinMode(MOSI, OUTPUT);
  digitalWrite(SCK, level);
  digitalWrite(MOSI, level);
  digitalWrite(SS, level);
  delay(300);
  digitalWrite(SCK, LOW);
  digitalWrite(MOSI, LOW);
  digitalWrite(SS, HIGH);
  SPI.begin();
  pinMode(SS, OUTPUT);
  digitalWrite(SS, HIGH);
  delayMicroseconds(200);

  bool ok = receiving && marcState() == MARC_RX && config(CC1101_ADDR) == 0x5A;
  rf.writeCommand(CC1101_SIDLE);
  return result(level ? "ground (pin 2 GND)" : "power (pin 1 VCC)", ok,
                level ? "radio lost power: GND pin or the G socket pin is not connected" : "radio lost power: VCC pin or the 3V3 socket pin is not connected");
}

// Frequency offset: listen with a receiver wide enough to decode packets from a module that is far off,
// and read how far off each packet was.
static const int8_t searchSteps[] = {0, 50, -50}; // extra FSCTRL0 tried in turn while nothing is received
static uint8_t searchIndex = 0;
static bool locked = false;
static int8_t offsets[31];
static uint8_t offsetCount = 0;
static uint32_t lastPacketOrSwitch = 0, lastNote = 0, packets = 0;

static void listen(int8_t extra)
{
  rf.init();
  rf.initReceive();
  rf.writeCommand(CC1101_SIDLE);
  waitForState(MARC_IDLE, 50);
  rf.writeRegister(CC1101_MDMCFG4, 0x88);          // 203 kHz receive filter instead of 101 kHz
  rf.writeRegister(CC1101_FOCCFG, 0x17);           // offset compensation up to +-BW/2
  rf.writeRegister(CC1101_FSCTRL0, (uint8_t)extra); // measure the bare module, without CC1101_FREQ_OFFSET
  rf.writeCommand(CC1101_SRX);
  waitForState(MARC_RX, 100);
}

static int median()
{
  int8_t sorted[31];
  memcpy(sorted, offsets, offsetCount);
  for (int i = 1; i < offsetCount; i++)
    for (int j = i; j > 0 && sorted[j - 1] > sorted[j]; j--)
    {
      int8_t swap = sorted[j];
      sorted[j] = sorted[j - 1];
      sorted[j - 1] = swap;
    }
  return sorted[offsetCount / 2];
}

void setup()
{
  Serial.begin(115200);
  WiFi.mode(WIFI_OFF);
  WiFi.forceSleepBegin();
  delay(300);
  Serial.println("\n\n=== CC1101 check ===");

  rf.init();
  delay(20);
  uint8_t partnum = status(CC1101_PARTNUM), version = status(CC1101_VERSION);
  Serial.printf("  chip id %02X/%02X\n", partnum, version);
  bool ok = result("chip answers", partnum == 0x00 && (version == 0x14 || version == 0x04 || version == 0x18), "no CC1101 on the SPI pins");
  ok = checkSpi() && ok;
  ok = checkIrqLine() && ok;
  ok = checkSupplyPin(LOW) && ok;
  ok = checkSupplyPin(HIGH) && ok;
  Serial.println(ok ? "Wiring and power are fine." : "Fix the failed checks first, the frequency reading below is not reliable until then.");

  Serial.println("\nListening for MAX! packets to measure the frequency offset. Trigger a device (turn a thermostat wheel) to speed it up.");
  listen(searchSteps[searchIndex]);
  lastPacketOrSwitch = lastNote = millis();
}

void loop()
{
  static bool inPacket = false;
  static long sum = 0;
  static int samples = 0;
  static uint32_t syncAt = 0;

  uint8_t packetStatus = status(CC1101_PKTSTATUS);
  bool syncFound = packetStatus & 0x08;
  uint32_t now = millis();

  if (syncFound)
  {
    if (!inPacket)
    {
      inPacket = true;
      sum = 0;
      samples = 0;
      syncAt = now;
    }
    if (now - syncAt >= 2) // let the estimate settle after the sync word
    {
      sum += (int8_t)status(CC1101_FREQEST);
      samples++;
    }
  }
  else
    inPacket = false;

  if (packetStatus & 0x04) // packet with good CRC is waiting
  {
    CC1101Packet packet;
    packet.length = 0;
    rf.receiveData(&packet);
    if (samples > 0 && packet.length >= 10)
    {
      int total = searchSteps[searchIndex] + (int)lround((double)sum / samples);
      total = constrain(total, -127, 127);
      if (offsetCount < sizeof(offsets))
        offsets[offsetCount++] = total;
      else
      {
        memmove(offsets, offsets + 1, sizeof(offsets) - 1);
        offsets[sizeof(offsets) - 1] = total;
      }
      packets++;
      locked = true;
      lastPacketOrSwitch = now;
      int suggested = median();
      Serial.printf("  packet %u from %02X%02X%02X, %d dBm, off by %+.1f kHz  =>  -DCC1101_FREQ_OFFSET=%d (median of %u, %+.1f kHz)\n",
                    packets, packet.data[4], packet.data[5], packet.data[6], toDbm(packet.data[packet.length - 2]),
                    total * KHZ_PER_STEP, suggested, offsetCount, suggested * KHZ_PER_STEP);
      if (packets == 5)
        Serial.println(abs(suggested) <= 6 ? "  This module is on frequency, it needs no correction."
                                             : "  This module needs the correction above, see docs/cc1101-shield.md.");
    }
    samples = 0;
  }

  if (marcState() != MARC_RX && !(status(CC1101_PKTSTATUS) & 0x08))
  {
    rf.writeCommand(CC1101_SIDLE);
    rf.writeCommand(CC1101_SFRX);
    rf.writeCommand(CC1101_SRX);
  }

  if (!locked && now - lastPacketOrSwitch > 90000)
  {
    searchIndex = (searchIndex + 1) % (sizeof(searchSteps) / sizeof(searchSteps[0]));
    listen(searchSteps[searchIndex]);
    lastPacketOrSwitch = now;
    Serial.printf("  nothing decoded, now listening %+.0f kHz from nominal\n", searchSteps[searchIndex] * KHZ_PER_STEP);
  }
  if (packets == 0 && now - lastNote > 30000)
  {
    lastNote = now;
    Serial.printf("  no packets yet, noise floor %d dBm\n", toDbm(status(CC1101_RSSI)));
  }
  delayMicroseconds(300);
}
#endif
