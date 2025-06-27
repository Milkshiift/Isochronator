# Isochronator

A simple isochronic tone and visual stimulus generator written in Rust.

This application generates a pulsing sound (isochronic tone), synchronized with a flashing screen.

## Features

*   **Isochronic Tones:** Generates a pure sine wave tone that pulses at a specified frequency.
*   **Visual Stimulus:** The screen flashes in sync with the audio tone. The brightness is anti-aliased for a smoother (and a more correct) visual effect.
*   **Configurable:** Set the primary frequency, base tone, on/off colors, audio ramp time, and volume from the command line.
*   **Cross-Platform:** Built with `winit`, `pixels`, and `cpal`, it should run on Windows, macOS, and Linux.

## Build & Run

1.  **Clone the repository:**
    ```sh
    git clone https://github.com/your-username/isochronator.git
    cd isochronator
    ```

2.  **Build and run the application:**
    For the best performance, run in release mode.

    ```sh
    cargo run --release
    ```

### Usage

Run with `--help` to see all options.

```
A simple isochronic tone and visual stimulus generator.

Usage: isochronator [OPTIONS]

Options:
  -f, --frequency <FREQUENCY>          The primary frequency of the isochronic tones in Hz [default: 20]
  -r, --ramp-duration <RAMP_DURATION>  The duration of the audio fade-in/out ramp in seconds. Low values may produce clicks [default: 0.005]
  -a, --amplitude <AMPLITUDE>          The audio volume (0.0 to 1.0) [default: 0.25]
  -t, --tone-hz <TONE_HZ>              The frequency of the audible sine wave tone in Hz [default: 440]
      --on-color <ON_COLOR>            The 'on' color of the screen flash (RRGGBB hex) [default: ffffff]
      --off-color <OFF_COLOR>          The 'off' color of the screen flash (RRGGBB hex) [default: 000000]
  -h, --help                           Print help
  -V, --version                        Print version
```

**Example:** Run a 10 Hz session with a 500 Hz base tone, a very soft/slow audio pulse and low volume.

```sh
cargo run --release -- -f 10 -t 500 --ramp-duration 0.05 --amplitude 0.1
```