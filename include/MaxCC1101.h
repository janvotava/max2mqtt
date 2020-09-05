#ifndef __MAXCC1101_H__
#define __MAXCC1101_H__

#include <stdio.h>
#include "CC1101.h"

class MaxCC1101 : public CC1101
{
	//functions
	public:
		MaxCC1101();
		~MaxCC1101();

		//init
		void init() { CC1101::init(); }
		void initReceive();
}; //MaxCC1101


extern volatile uint32_t data1[];

#endif //__MAXCC1101_H__
