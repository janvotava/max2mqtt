/*
 * Based on Klusjesman work
 */

#include "CC1101.h"

// default constructor
CC1101::CC1101()
{
	SPI.begin();
#ifdef ESP8266
	pinMode(SS, OUTPUT);
#endif
} //CC1101

// default destructor
CC1101::~CC1101()
{
} //~CC1101

/***********************/
// SPI helper functions select() and deselect()
inline void CC1101::select(void)
{
	digitalWrite(SS, LOW);
}

inline void CC1101::deselect(void)
{
	digitalWrite(SS, HIGH);
}

void CC1101::spi_waitMiso()
{
	while (digitalRead(MISO) == HIGH)
		yield();
}

void CC1101::init()
{
	reset();
}

void CC1101::reset()
{
	deselect();
	delayMicroseconds(5);
	select();
	delayMicroseconds(10);
	deselect();
	delayMicroseconds(45);
	select();

	spi_waitMiso();
	SPI.transfer(CC1101_SRES);
	delay(10);
	spi_waitMiso();
	deselect();
}

uint8_t CC1101::writeCommand(uint8_t command)
{
	uint8_t result;

	select();
	spi_waitMiso();
	result = SPI.transfer(command);
	deselect();

	return result;
}

void CC1101::writeRegister(uint8_t address, uint8_t data)
{
	select();
	spi_waitMiso();
	SPI.transfer(address);
	SPI.transfer(data);
	deselect();
}

uint8_t CC1101::readRegister(uint8_t address)
{
	uint8_t val;

	select();
	spi_waitMiso();
	SPI.transfer(address);
	val = SPI.transfer(0);
	deselect();

	return val;
}

/* Known SPI/26MHz synchronization bug (see CC1101 errata)
This issue affects the following registers: SPI status byte (fields STATE and FIFO_BYTES_AVAILABLE),
FREQEST or RSSI while the receiver is active, MARCSTATE at any time other than an IDLE radio state,
RXBYTES when receiving or TXBYTES when transmitting, and WORTIME1/WORTIME0 at any time.*/
//uint8_t CC1101::readRegisterWithSyncProblem(uint8_t address, uint8_t registerType)
uint8_t /* ICACHE_RAM_ATTR */ CC1101::readRegisterWithSyncProblem(uint8_t address, uint8_t registerType)
{
	uint8_t value1, value2;

	value1 = readRegister(address | registerType);

	//if two consecutive reads gives us the same result then we know we are ok
	do
	{
		value2 = value1;
		value1 = readRegister(address | registerType);
	} while (value1 != value2);

	return value1;
}

//registerType = CC1101_CONFIG_REGISTER or CC1101_STATUS_REGISTER
uint8_t CC1101::readRegister(uint8_t address, uint8_t registerType)
{
	switch (address)
	{
	case CC1101_FREQEST:
	case CC1101_MARCSTATE:
	case CC1101_RXBYTES:
	case CC1101_TXBYTES:
	case CC1101_WORTIME1:
	case CC1101_WORTIME0:
		return readRegisterWithSyncProblem(address, registerType);

	default:
		return readRegister(address | registerType);
	}
}

void CC1101::writeBurstRegister(uint8_t address, uint8_t *data, uint8_t length)
{
	uint8_t i;

	select();
	spi_waitMiso();
	SPI.transfer(CC1101_WRITE_BURST | address);
	// SPI.writeBytes(data, length);
	for (i = 0; i < length; i++)
	{
		SPI.transfer(data[i]);
	}
	deselect();
}

void CC1101::readBurstRegister(uint8_t *buffer, uint8_t address, uint8_t length)
{
	uint8_t i;

	select();
	spi_waitMiso();
	SPI.transfer(address | CC1101_READ_BURST);

	for (i = 0; i < length; i++)
	{
		buffer[i] = SPI.transfer(0x00);
	}

	deselect();
}

uint8_t CC1101::receiveData(CC1101Packet *packet)
{
	uint8_t rxBytes = readRegisterWithSyncProblem(CC1101_RXBYTES, CC1101_STATUS_REGISTER);
	rxBytes = rxBytes & CC1101_BITS_RX_BYTES_IN_FIFO;
	if (rxBytes && readRegisterWithSyncProblem(CC1101_MARCSTATE, CC1101_STATUS_REGISTER) & CC1101_BITS_MARCSTATE != CC1101_MARCSTATE_RXFIFO_OVERFLOW)
	{
		readBurstRegister(packet->data, CC1101_RXFIFO, rxBytes);
		packet->length = rxBytes;
	}
	else
	{
		packet->length = 0;
	}

	uint8_t MarcState;
	MarcState = (readRegisterWithSyncProblem(CC1101_MARCSTATE, CC1101_STATUS_REGISTER) & CC1101_BITS_MARCSTATE);

	if (MarcState == CC1101_MARCSTATE_RXFIFO_OVERFLOW)
	{
		Serial.println("underflow detected");

		writeCommand(CC1101_SIDLE); //idle
		writeCommand(CC1101_SFRX);	//flush RX buffer
		writeCommand(CC1101_SRX);		//switch to RX state
	}

	return packet->length;
}

void CC1101::sendData(CC1101Packet *packet, bool longPreamble)
{
	uint8_t index = 0;
	uint8_t MarcState;
	uint8_t length;

	length = (packet->length <= CC1101_DATA_LEN ? packet->length : CC1101_DATA_LEN);
	writeCommand(CC1101_SIDLE); //idle
	writeCommand(CC1101_SFTX);	//flush TX buffer
	writeCommand(CC1101_SIDLE);
	writeCommand(CC1101_STX);

	if (longPreamble)
	{
		delay(1000); // Long preamble
	}
	writeBurstRegister(CC1101_TXFIFO, packet->data, length);

	do
	{
		MarcState = (readRegisterWithSyncProblem(CC1101_MARCSTATE, CC1101_STATUS_REGISTER) & CC1101_BITS_MARCSTATE);
		delay(10);

		if (MarcState == CC1101_MARCSTATE_TXFIFO_UNDERFLOW)
		{
			writeCommand(CC1101_SIDLE); //idle
			writeCommand(CC1101_SFTX);	//flush TX buffer
			writeCommand(CC1101_SIDLE); //idle
			writeCommand(CC1101_SRX);
		}
	} while ((MarcState != CC1101_MARCSTATE_IDLE) && (MarcState != CC1101_MARCSTATE_RX));
}
