#include "MaxCC1101.h"
#include <string.h>
#include <Arduino.h>
#include <SPI.h>

// default constructor
MaxCC1101::MaxCC1101() : CC1101()
{
}

// default destructor
MaxCC1101::~MaxCC1101()
{
} //~MaxCC1101

void MaxCC1101::initReceive()
{
  writeCommand(CC1101_SCAL);

  //wait for calibration to finish
  while ((readRegisterWithSyncProblem(CC1101_MARCSTATE, CC1101_STATUS_REGISTER)) != CC1101_MARCSTATE_IDLE)
    delay(100);

  writeRegister(CC1101_IOCFG2, 0x07); //GDO2_CFG=7: Asserts when a packet has been received with CRC OK. De-asserts when the first byte is read from the RX FIFO
  writeRegister(CC1101_IOCFG0, 0x46);
  writeRegister(CC1101_SYNC1, 0xC6);
  writeRegister(CC1101_SYNC0, 0x26);
  writeRegister(CC1101_FSCTRL1, 0x06);
  writeRegister(CC1101_MDMCFG4, 0xC8); //DRATE_E=8,
  writeRegister(CC1101_MDMCFG3, 0x93); //DRATE_M=147, data rate = (256+DRATE_M)*2^DRATE_E/2^28*f_xosc = (9992.599) 1kbit/s (at f_xosc=26 Mhz)
  writeRegister(CC1101_MDMCFG2, 0x03);
  writeRegister(CC1101_MDMCFG1, 0x22); //CHANSPC_E=2, NUM_PREAMBLE=2 (4 bytes), FEC_EN = 0 (disabled)
  writeRegister(CC1101_DEVIATN, 0x34);
  writeRegister(CC1101_MCSM1, 0x3F); //TXOFF=RX, RXOFF=RX, CCA_MODE=3:If RSSI below threshold unless currently receiving a packet
  writeRegister(CC1101_MCSM0, 0x28); //PO_TIMEOUT=64, FS_AUTOCAL=2: When going from idle to RX or TX automatically
  writeRegister(CC1101_FOCCFG, 0x16);
  writeRegister(CC1101_AGCCTRL2, 0x43);
  writeRegister(CC1101_FREND1, 0x56);
  writeRegister(CC1101_FSCAL1, 0x00);
  writeRegister(CC1101_FSCAL0, 0x11);
  writeRegister(CC1101_FREQ2, 0x21);
  writeRegister(CC1101_FREQ1, 0x65);
  writeRegister(CC1101_FREQ0, 0x6A);
  writeRegister(CC1101_PKTCTRL1, 0x0C);
  writeRegister(CC1101_MCSM2, 0x07);   //RX_TIME = 7 (Timeout for sync word search in RX for both WOR mode and normal RX operation = Until end of packet) RX_TIME_QUAL=0 (check if sync word is found)
  writeRegister(CC1101_WORCTRL, 0xF8); //WOR_RES=00 (1.8-1.9 sec) EVENT1=7 (48, i.e. 1.333 – 1.385 ms)
  writeRegister(CC1101_WOREVT1, 0x87); //EVENT0[high]
  writeRegister(CC1101_WOREVT0, 0x6B); //EVENT0[low]
  writeRegister(CC1101_FSTEST, 0x59);
  writeRegister(CC1101_TEST2, 0x81);
  writeRegister(CC1101_TEST1, 0x35);
  writeRegister(CC1101_PATABLE, 0xC3);
  writeRegister(CC1101_PKTLEN, 30); // Max length 30 bytes

  writeCommand(CC1101_SCAL);

  while ((readRegisterWithSyncProblem(CC1101_MARCSTATE, CC1101_STATUS_REGISTER)) != CC1101_MARCSTATE_IDLE)
    delay(10);

  writeCommand(CC1101_SIDLE);
  writeCommand(CC1101_SRX);

  while ((readRegisterWithSyncProblem(CC1101_MARCSTATE, CC1101_STATUS_REGISTER)) != CC1101_MARCSTATE_RX)
    delay(10);
}
