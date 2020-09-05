#include "Arduino.h"
#include "state.h"

void format();
void parseDeviceConfigFile(state *device, String filename);
bool loadConfig();
bool saveConfig();