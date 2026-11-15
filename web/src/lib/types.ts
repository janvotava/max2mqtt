// Shapes of the firmware's JSON API (esp/src/web.rs, bridge Bridge::status).

export type DeviceType = 'cube' | 'heater' | 'heater_plus' | 'thermostat' | 'shutter_contact' | 'push_button';
export type Mode = 'auto' | 'manual' | 'temporary' | 'boost';
export const DAYS = ['monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday', 'sunday'] as const;
export type Day = (typeof DAYS)[number];

export interface DeviceStatus {
	address: string;
	name: string;
	type?: DeviceType;
	room?: string;
	mode?: Mode;
	measured_temperature?: number;
	desired_temperature?: number;
	valve_position?: number;
	open?: boolean;
	low_battery?: boolean;
	rf_error?: boolean;
	rssi?: number;
	seen_seconds_ago?: number;
	/** Settings sent but not acknowledged yet. */
	unconfirmed: number;
}

export interface Status {
	bridge: {
		availability: string;
		address: string;
		pairing_enabled: boolean;
		pairing_seconds_left?: number;
		autocreate: boolean;
		furnace_running: boolean;
		heat_demand: number;
		tx_queue: number;
		booted_at?: string;
		duty_cycle_limit: boolean;
		credit_ms: number;
		/** Commands parked until a sleeping device (window sensor) wakes up. */
		tx_waiting: number;
		time?: string;
		timezone: { utc_offset_minutes: number; eu_dst: boolean };
		heating: Heating;
	};
	devices: DeviceStatus[];
	network: {
		station: boolean;
		ssid: string;
		ip: string | null;
		hostname: string;
		hotspot: boolean;
		hotspot_ssid: string;
		mqtt: boolean;
		mqtt_host: string;
	};
	system: {
		version: string;
		board: string;
		uptime_seconds: number;
		heap_used: number;
		heap_max: number;
		heap_size: number;
		firmware_slot: string;
		firmware_state: string;
	};
}

/** As returned by GET /api/settings: passwords replaced by `password_set`. */
export interface Settings {
	hostname: string;
	password_set: boolean;
	wifi: { ssid: string; password_set: boolean };
	mqtt: {
		host: string;
		port: number;
		user: string;
		password_set: boolean;
		prefix: string;
		homeassistant: boolean;
		discovery_prefix: string;
	};
	ntp_server: string;
	cc1101_freq_offset: number;
}

export interface Network {
	ssid: string;
	rssi: number;
	secure: boolean;
}

export interface Temperatures {
	comfort: number;
	eco: number;
	max: number;
	min: number;
	window_open: number;
	window_open_minutes: number;
	measurement_offset: number;
}

export interface DeviceConfig {
	address: string;
	name: string;
	room?: string;
	type?: DeviceType;
	group: number;
	associations?: string[];
	temperatures: Temperatures;
	/** Unconfirmed settings: "group", "temperatures", { link: address }, { program: day }, … */
	pending?: (string | Record<string, string | string[]>)[];
	/** Day name to start time ("HH:MM") to °C. */
	schedule?: Partial<Record<Day, Record<string, number>>>;
}

export interface BridgeConfig {
	address: string;
	autocreate: boolean;
	duty_cycle_limit: boolean;
	devices: DeviceConfig[];
}

export interface Heating {
	cycle_minutes: number;
	min_on_minutes: number;
	min_off_minutes: number;
	min_valve_position: number;
	stale_minutes: number;
}

export interface Crash {
	time?: number;
	uptime_seconds: number;
	version: string;
	board: string;
	reason: string;
	backtrace: string[];
}
