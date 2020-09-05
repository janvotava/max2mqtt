#include "CC1101Packet.h"

typedef struct
{
  bool sent;
  CC1101Packet packet;
  byte retryCounter;
  byte msgcnt;
  bool longPreamble;
  bool waitForAck;
} Message;
