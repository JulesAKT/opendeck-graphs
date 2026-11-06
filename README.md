# Hardware Graphs

A configurable OpenDeck plugin that displays real-time hardware sensor data as interactive graphs.

![Hardware Graphs Plugin](img/readme.png)

## Features

- Monitor CPU, GPU, RAM, and other hardware sensors
- Display metrics as live timeseries graphs on your Stream Deck
- Fully configurable to track the sensors you care about

## Installation

Install via OpenDeck plugin manager or manually place in your plugins directory.

## Usage

Add the Hardware Graph action to any key and configure which sensor to monitor through the property inspector.

## Platform support

Runs on Linux (x86_64, aarch64) and on macOS with Apple Silicon.

On macOS, the property inspector only offers metrics that have a sensor. GPU temperature, GPU power, motherboard temperature, fan speed, CPU voltage and RAM temperature aren't available. Some metrics also read differently:

- **CPU temperature** is the hottest SoC die sensor, because Apple doesn't document which sensors cover the CPU cores.
- **GPU memory** is shown against total system RAM, because Apple Silicon has no dedicated VRAM.
- **Network speed** only counts `en*` interfaces, so VPN traffic isn't counted twice.

## Experimental

There is a WebSockets usecase that this plugin supports where it can retrieve data from a ws endpoint but might not work properly.
