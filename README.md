# Isochronator

A simple isochronic tone and binaural beat generator with visual stimulus written in Rust.

This application generates a pulsing sound (isochronic tone) or a binaural beat, synchronized with a flashing screen.

## Features

*   **Isochronic Tones:** Generates a pure sine wave tone that pulses at a specified frequency.
*   **Binaural Beats:** Generates two slightly different pure sine wave tones, one for each ear, creating the perception of a third "beat" frequency within the brain.
*   **Visual Stimulus:** The screen flashes in sync with the audio tone. The brightness is anti-aliased for a smoother (and a more correct) visual effect.
*   **Configurable:** Set the primary frequency, base tone, on/off colors, audio ramp time, and volume from the command line.
*   **Cross-Platform:** Built with `winit`, `pixels`, and `cpal`, it should run on Windows, macOS, and Linux.

> [!CAUTION]
> ## HEALTH AND SAFETY WARNING
>
> This software produces intense flashing lights that can trigger seizures in people with photosensitive epilepsy (PSE). You may have this condition without knowing it.
>
> ### Safe Use Guidelines
> *   **Never use this application when you are alone.**
> *   Start with a small window in a well-lit room. Do not use fullscreen.
> *   If you feel dizzy, unwell, or experience any strange visual effects, **stop immediately**.
> *   For a risk-free, audio-only experience, use the `--headless` flag.
>
> **You assume all health risks by using this software. The developer is not liable for any harm caused.**

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
A simple isochronic/binaural tone and visual stimulus generator.

Usage: isochronator [OPTIONS]

Options:
  -f, --frequency <FREQUENCY>          The primary frequency of the isochronic tones in Hz. In binaural mode, this becomes the beat frequency [default: 20]
  -r, --ramp-duration <RAMP_DURATION>  The duration of the audio fade-in/out ramp in seconds. Low values may produce clicks [default: 0.005]
  -a, --amplitude <AMPLITUDE>          The audio volume (0.0 to 1.0) [default: 0.25]
  -t, --tone-hz <TONE_HZ>              The frequency of the audible sine wave tone in Hz [default: 440]
  -b, --binaural                       Enable binaural beat mode instead of isochronic tones
      --on-color <ON_COLOR>            The 'on' color of the screen flash (RRGGBB hex) [default: ffffff]
      --off-color <OFF_COLOR>          The 'off' color of the screen flash (RRGGBB hex) [default: 000000]
      --headless                       Run in headless mode (audio only, no visuals)
      --headless-profile               Run in a headless mode for a few seconds to generate PGO profile data (no audio output)
  -h, --help                           Print help
  -V, --version                        Print version
```

**Isohcronic Tone Example:** Run a 10 Hz session with a 500 Hz base tone, a very soft/slow audio pulse and low volume.

```sh
cargo run --release -- -f 10 -t 500 --ramp-duration 0.05 --amplitude 0.1
```

**Binaural Beat Example:** Run a 6 Hz session with a 200 Hz base tone

```sh
cargo run --release -- -f 6 -t 200 --binaural
```

If running from a prebuilt binary, replace `cargo run --release --` with `./isochronator`