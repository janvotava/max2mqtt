# MAX2MQTT

Quick and dirty eQ-3 MAX! to MQTT bridge using ESP8266. I'm running it on Wemos D1 mini with relay shield
used to activate my gas furnace when heating is needed.

Mapping of CC1101 to ESP8266 pins is as follows, custom shield PCB to come.

```
CC11xx pins    ESP pins   Description
1 - VCC        VCC        3v3
2 - GND        GND        Ground
3 - MOSI       13=D7      Data input to CC11xx
4 - SCK        14=D5      Clock pin
5 - MISO/GDO1  12=D6      Data output from CC11xx / serial clock from CC11xx
6 - GDO2       04=D2      Serial data to CC11xx
8 - CSN        15=D8      Chip select / (SPI_SS)
```

## Usage

```bash
git clone git@github.com:janvotava/max2mqtt.git
cd max2mqtt
git submodule update --init --recursive
cp configuration_sample.h configuration.h
```

And edit configuration file, then upload via Arduino with at least 1M spiffs.

## Configuration

Can be done via MQTT.

```bash
HOSTNAME="mqtt.lan"

mosquitto_pub -h $HOSTNAME -t max/set -m '{"address":"DEDABA"}'
mosquitto_pub -h $HOSTNAME -t max/rename -m '{"address":"123456","to":"living-room/wall-thermostat"}'
mosquitto_pub -h $HOSTNAME -t max/rename -m '{"address":"234567","to":"living-room/heater"}'
mosquitto_pub -h $HOSTNAME -t max/living-room/wall-thermostat/set -m '{"group":2,"room":"living-room","eco_temperature":4.5,"comfort_temperature":21.5,"display_actual_temperature":true,"boost_duration":30}'
mosquitto_pub -h $HOSTNAME -t max/living-room/heater/set -m '{"group":2,"room":"living-room","associate":"living-room/wall-thermostat","eco_temperature":4.5,"comfort_temperature":21.5,"decalc_weekday":"saturday","decalc_hour":12,"boost_duration":30}'
mosquitto_pub -h $HOSTNAME -t max/living-room/wall-thermostat/set -m '{"day":"monday","schedule":{"6:00":21.5,"22:30":4.5}}'
```

## TODO
- documentation
- get rid of hardcoded configuration
  - minimum valve position
  - timezone
  - etc.
- cleanup
- optimizations
  - save just config files that we're changed
  - etc.
- iplement rest of the features
  - push button
  - etc.
- CC1101 shield PCB
- 3D printed case
