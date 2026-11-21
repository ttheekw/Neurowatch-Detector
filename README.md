# NeuroWatch Detect (nw-detect)

[![CI](https://github.com/ttheekw/Neurowatch-Detector/actions/workflows/ci.yml/badge.svg)](https://github.com/ttheekw/Neurowatch-Detector/actions/workflows/ci.yml)
[![Release](https://github.com/ttheekw/Neurowatch-Detector/actions/workflows/release.yml/badge.svg)](https://github.com/ttheekw/Neurowatch-Detector/releases/latest)

The object detection plugin for [NeuroWatch](https://github.com/ttheekw/Neurowatch): it finds
**people, vehicles and animals** in your cameras' video and reports each as a NeuroWatch event,
with a box around each one on the live wall and a count of what is in view. Events work like any
other: the Events tab, alerts, event-based recording, and Home Assistant through NeuroWatch's
MQTT settings.

NeuroWatch sends it the cameras' compressed H.264/H.265 video (or pictures the recorder already
decodes for a screen); the plugin decodes and analyses the video itself, on the CPU, with a small
[YOLOX](https://github.com/Megvii-BaseDetection/YOLOX) model (Apache-2.0) built in. There is no
native AI runtime to install: inference runs on [tract](https://github.com/sonos/tract), a
pure-Rust ONNX runtime. Apache-2.0, like NeuroWatch.

### Why a separate program

Recording is the one job a recorder must never fail at, and AI detection is its heaviest and most
often changed part. So NeuroWatch keeps detection out of the recording process:

- **A crash never stops recording.** If `nw-detect` stops, NeuroWatch reports the plugin offline
  and keeps recording every camera.
- **A slow detector never holds up recording.** It reads the video through its own bounded queue;
  when it falls behind, it skips pictures instead of making the camera or recording wait.
- **Update or switch it off at any time** without restarting the recorder or interrupting
  footage.
- **Run it somewhere else.** It can run on another computer, connected over encrypted QUIC or
  TLS, so a busy recorder does not have to do the detection.

## Download

The [release workflow](.github/workflows/release.yml) builds packages with their own GStreamer
and FFmpeg decoders. The links below become available after the first successful release;
until then, [build from source](#run-from-source).

| System | Download |
|---|---|
| Windows 10/11 (64-bit), automatic startup | [nw-detect-windows-x86_64-setup.exe](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect-windows-x86_64-setup.exe) |
| Windows 10/11 (64-bit), portable/manual | [nw-detect-windows-x86_64.zip](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect-windows-x86_64.zip) |
| Ubuntu 22.04+, Debian 12+ (64-bit PC) | [nw-detect_amd64.deb](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect_amd64.deb) |
| Ubuntu 22.04+, Debian 12+ (ARM 64-bit) | [nw-detect_arm64.deb](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect_arm64.deb) |
| Fedora and other RPM systems (64-bit PC) | [nw-detect.x86_64.rpm](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect.x86_64.rpm) |
| Fedora and other RPM systems (ARM 64-bit) | [nw-detect.aarch64.rpm](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect.aarch64.rpm) |
| Arch Linux | [nw-detect-x86_64.pkg.tar.zst](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect-x86_64.pkg.tar.zst) |
| Any other Linux (64-bit PC), portable/manual | [nw-detect-linux-x86_64.tar.gz](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect-linux-x86_64.tar.gz) |
| Any other Linux (ARM 64-bit), portable/manual | [nw-detect-linux-arm64.tar.gz](https://github.com/ttheekw/Neurowatch-Detector/releases/latest/download/nw-detect-linux-arm64.tar.gz) |

Older versions, release notes and `SHA256SUMS`: [all releases](https://github.com/ttheekw/Neurowatch-Detector/releases).
Linux packages target glibc 2.35 or newer and require the host's GLib and standard runtime
libraries (installed automatically by native packages). The release workflow checks ARM builds
in Ubuntu 22.04, Debian 12/13 and Fedora containers; 64-bit Raspberry Pi OS (bookworm) meets the requirement but has not been
tried on a Pi.

### Install

```sh
sudo apt install ./nw-detect_amd64.deb          # Ubuntu, Debian
sudo dnf install ./nw-detect.x86_64.rpm         # Fedora
sudo pacman -U nw-detect-x86_64.pkg.tar.zst     # Arch Linux
sha256sum -c SHA256SUMS --ignore-missing        # optional: check the download
```

**Linux packages:** install to `/opt/nw-detect`, put `nw-detect` on the PATH, and install a
systemd user service. It starts in existing user sessions at installation and at subsequent
sign-ins, and restarts if it crashes. It runs as your account, using the default private local
socket that NeuroWatch's local discovery scans. No `--listen` command or `systemctl enable`
step is required. A systemd user session is required for automatic startup.

**Windows installer:** run the setup executable. It installs for your account, starts the
daemon without a console window, and adds a sign-in shortcut. The default local named pipe
is available to NeuroWatch's local discovery. Uninstalling removes the shortcut and stops
that installed copy; upgrades preserve its identity.

**Portable downloads:** extract the tarball or ZIP and run `nw-detect/nw-detect` on Linux or
`nw-detect\bin\nw-detect.exe` on Windows. These are the manual option and do not install startup
entries. `nw-detect --check` checks the bundled decoders.

### Check a download

`SHA256SUMS` lists every file of a release, and the release workflow signs it with
[Sigstore cosign](https://docs.sigstore.dev/cosign/signing/overview/). No signing key is stored
anywhere: the signature, certificate and transparency-log proof are in
`SHA256SUMS.sigstore.json` beside it. Download both next to the package, then, with cosign 3
or newer:

```sh
sha256sum -c SHA256SUMS --ignore-missing
cosign verify-blob --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp '^https://github\.com/ttheekw/Neurowatch-Detector/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS
```

The first command shows the file is the one the release lists; the second that the list was
made by this repository's release workflow for a version tag.

### Local mode (default)

The daemon listens locally by default and is found in NeuroWatch's **Plugins → Add Plugin →
Local**; no address needs entering. Approve it (check the key fingerprint it prints) and bind
cameras there. Local discovery does not bypass the app's approval or camera-binding controls.
It runs beside the included motion plugin, each with its own socket. No network port is opened
by the installers.

### Remote mode (optional)

On the remote computer, run a separate daemon with an explicit network listener (open UDP
port 7690, or TCP for `tls:`), give it its own persistent identity and an access code file (see
[Access codes](#access-codes)):

```sh
nw-detect --listen quic:0.0.0.0:7690 --identity ~/.local/state/nw-detect/remote.key \
    --access-codes ~/.local/state/nw-detect/codes.json
```

A recorder needs nothing but the address to use the daemon (it proves only its own key), so on an
address other machines can reach the daemon refuses to start without `--access-codes`. Add
`--open` to serve every recorder that connects instead.

In NeuroWatch open **Plugins → Add Plugin → URL**, enter `quic://that-computer:7690`, check that
the fingerprint shown matches the one the daemon logs, and approve it. Then bind cameras on the
plugin's page. Keep the identity file: a new key needs approving again. `tls:` works where UDP is
blocked.

## Run from source

Install Rust 1.88 or newer, a C/C++ build toolchain, pkg-config, and GStreamer development
libraries. On Ubuntu 24.04:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libgstreamer1.0-dev \
    libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-base \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

Source builds run in the foreground unless you arrange startup yourself; the native installers
provide automatic startup. On Windows, use an MSVC developer shell and GStreamer's MSVC runtime and development
installation; set `GSTREAMER_1_0_ROOT_MSVC_X86_64`, add its `bin` directory to `PATH`, and
set `PKG_CONFIG_PATH` to its `lib/pkgconfig` directory.

```sh
cargo build --release --locked
target/release/nw-detect --check
# Default: private Unix socket, or local Windows named pipe.
target/release/nw-detect
# Remote encrypted transport with a persistent identity (and access codes, or --open):
target/release/nw-detect --listen quic:0.0.0.0:7690 --identity /private/path/detect.key \
    --access-codes /private/path/codes.json
# Encrypted fallback when UDP is blocked:
target/release/nw-detect --listen tls:0.0.0.0:7690 --identity /private/path/detect.key \
    --access-codes /private/path/codes.json
# Loopback development:
target/release/nw-detect --listen tcp:127.0.0.1:7690
```

The two required analytics libraries are included under [`vendor/`](vendor/README.md), so
building from source does not require a NeuroWatch checkout. Use GStreamer >=1.24 with
h264parse/h265parse and gst-libav (the release packages bring their own). When a sibling
NeuroWatch checkout is present, a binary started by hand can use that checkout's release GStreamer
(`.cache/gstreamer-sdk-<newest>`, from `scripts/install_gstreamer_dev_sdk.sh`) when it is there,
as NeuroWatch runs its backend: Ubuntu 24.04's GStreamer 1.24.2 logs a
`gst_mini_object_unref` critical for every picture the analysis skips. `NW_GSTREAMER_SDK=0`
keeps the system's, a directory names another SDK; a set `GST_PLUGIN_SYSTEM_PATH_1_0` wins. Each camera's **Video decoder**
setting (Automatic, VA-API, NVIDIA NVDEC, Intel Quick Sync, V4L2 or CPU; Direct3D on Windows)
names the decoder used. Automatic tries VA-API, NVDEC, Direct3D 11, then V4L2, as NeuroWatch
does; a choice that cannot be used, or a hardware decoder with no picture after 10 s, moves on
and ends on the CPU, with the reason in the camera's status. VA-API is refused on GStreamer
1.24.0–1.24.5 (it leaks). `--hardware-decoding` is ignored. `--check` lists the hardware decoders
found. Preserve the identity file across restarts. Unix
socket directories are private (0700), sockets and secret files owner-only (0600); Windows uses
local named pipes and DPAPI-protected keys. QUIC binds its TLS certificate to the same plugin key.
The host must approve that key and permissions before sending media or credentials.

An external daemon is never killed by the recorder. Up to 1024 camera bindings may be attached
per session, subject to actual CPU/decoder capacity; callbacks and media are bounded.

## Access codes

Without a code file, every recorder that reaches the daemon may use it: the recorder approves
the plugin's key, but the plugin does not approve recorders. That is fine for the local socket
(this user only); on a network address the daemon needs `--access-codes`, or `--open` to say
that everyone is welcome. To admit only recorders that know a code, and only so many at a time,
write a code file and start the daemon with it:

```sh
target/release/nw-detect access-codes add /private/path/codes.json site-a --max-connections 10
# prints a new random code once; or bring your own:
echo "$CODE" | target/release/nw-detect access-codes add /private/path/codes.json site-b \
    --max-connections 2 --code-stdin
target/release/nw-detect --listen quic:0.0.0.0:7690 --identity /private/path/detect.key \
    --access-codes /private/path/codes.json
```

The file never holds a code: only a random salt and, per code, its PBKDF2-HMAC-SHA256 hash
(600,000 rounds), its name and its limit. It is written owner-only (0600). `access-codes list`
shows names and limits, `limit PATH NAME N` changes a limit and `remove PATH NAME` removes a
code. Giving an existing name a new code with `add` replaces the old one.

A recorder sends its code as the plugin's vendor credential (NeuroWatch: Plugins → Vendor
Credential…). Each connected recorder holds one place under its code from sign-in until it
disconnects; probes from the Add Plugin dialog take none. A missing or unknown code is refused
with `ACCESS_CODE`, one over the limit with `CONNECTION_LIMIT`; NeuroWatch shows the message on
the plugin's page and tries again every 2 s, so it gets in once a place is free.

- **Edits apply while running.** The daemon reads the file again every 2 s. A removed code, or
  a name given a new code, disconnects the recorders using it; a lowered limit disconnects the
  newest connections above it. A file that is broken, missing or unreadable keeps the list in
  force (one warning per version of the file); at start it stops the daemon instead of serving
  everyone.
- **Lost recorders give their place back.** NeuroWatch pings every 5 s; a connection silent for
  15 s (a lost network can leave one open for minutes) is closed and its place freed.
- **Guessing is slowed.** After 5 wrong codes within a minute, each further refusal is answered
  250 ms later than the one before (at most 5 s), whoever sends it. A correct code is never
  delayed.
- **Restarts start from zero.** Places are kept in memory only: after a restart every recorder
  reconnects within seconds and is counted again. If more recorders share a code than its
  limit, which of them get in after a restart is first come, first served.

## What it reports, and its settings

Bind a camera in **Plugins** (NeuroWatch or the web portal), then set, per camera:

| Setting | Default | What it does |
|---|---|---|
| Report | People, Vehicles | Which of people, vehicles (bicycle, car, motorcycle, bus, truck) and animals (bird, cat, dog, horse, sheep, cow and others) to look for |
| Confidence | 40% | How sure the model must be. Higher: fewer false alarms, small or distant objects missed |
| Minimum duration | 1 s | Something seen for less is not reported |
| Ends after | 10 s | An event ends once nothing of its kind has been seen this long |
| Pictures analysed per second | 2 | More catches quick visits and uses more processor |
| Keyframes only (low CPU) | off | About one picture every one or two seconds |
| Video decoder | Automatic | Hardware decoders first, falling back to the CPU |
| Watched areas | whole picture | Include and ignore zones; an object counts where it stands (the bottom of its box) |

Each kind is its own event (`person`, `vehicle`, `animal`): one event lasts while any of that kind
is in view. Drawings: `objects` (a labelled box per object, on by default) and `zones`. Figures
beside the picture count what is in view now.

**Speed.** On the development PC (a laptop CPU) one picture takes about 40 ms with the built-in
YOLOX-Nano, so two pictures a second per camera use under a tenth of one core. This is a single
measurement, not a benchmark: run `nw-detect --check` to time it on your machine.
`--model PATH` loads a larger YOLOX export, such as YOLOX-Tiny, for better accuracy on small
objects at about three times the work; see [models/README.md](models/README.md).

**Accuracy.** A small model trained on everyday photos: good on people and vehicles at normal
CCTV distances in daylight, weaker on small, distant or night-time objects. Raise the confidence
or use zones where it reports things that are not there.

## Develop

`nw-analytics-protocol` and `nw-analytics-sdk` are included library snapshots with explicit
dependencies; see [`vendor/README.md`](vendor/README.md) for their origin and update procedure.
`src/detector.rs` loads the model and turns pictures into detections, `activity.rs` turns
detections into events, `drawing.rs` the boxes and counts, `worker.rs` owns decoding and shared
pictures, `server.rs` the authenticated protocol. This repository started as a copy of
[NeuroWatch Motion](https://github.com/ttheekw/Neurowatch-Motion), whose transport, decoding and
packaging it shares.

```sh
cargo fmt --all -- --check
env -u GIO_MODULE_DIR cargo test --workspace --locked
cargo clippy --all-targets --locked -- -D warnings
python3 -m unittest discover -s tests -p 'test_*.py' -v
# A real picture through the whole daemon (any photo with a person in it):
NW_DETECT_TEST_PICTURE=/path/to/person.jpg cargo test --test daemon a_person
# The detector on one picture:
cargo run --release --example detect_image -- /path/to/picture.jpg
```

## Releasing

1. Set the new version in `Cargo.toml` and run `cargo build` (updates `Cargo.lock`).
2. Pin `neurowatch.ref` to a public NeuroWatch commit containing the GStreamer build
   (`scripts/build_release_gstreamer.sh`). Analytics libraries come from this repo's `vendor/`.
3. Push, wait for CI, then tag: `git tag v0.1.1 && git push origin v0.1.1`.

Pushing `main` or opening a pull request runs source checks and tests. Downloadable executables,
installers and Linux packages are built and published **only when a version tag is pushed**
(for example, `v0.1.1`). Normal branch pushes do not produce downloadable packages.

To try the packaging without releasing, run the workflow by hand: **Actions → Release → Run
workflow**, choose the branch and, if you like, enter a version such as `v0.1.1-rc.1` (left
empty, it is the one in `Cargo.toml`). It builds and checks every package exactly as a tag
does and publishes nothing: the packages stay on the run's page as artifacts.

The [Release workflow](.github/workflows/release.yml) builds the Windows installer/ZIP and Linux
packages (x86_64 and arm64), installs each one in clean Ubuntu, Debian, Fedora and Arch
containers, runs `--check`, serves with an access code, and only then publishes the GitHub
Release with a signed `SHA256SUMS` ([Check a download](#check-a-download)) and the version-free
names the download table links to. A tag with a suffix (`v0.2.0-rc.1`) is published as a
pre-release. The containers are pulled by digest (the `digest` beside each image in the
workflow): refresh a tag and its digest together. The pinned NeuroWatch revision is public;
no cross-repository token is required.

The [Windows workflow](.github/workflows/windows.yml) is called only by the release workflow
(a tag or a manual run). It checks installer startup at the default local pipe, the sign-in shortcut, identity preservation
on upgrade, and shutdown/shortcut removal on uninstall. Linux native packages include the
automatically enabled user service; package smoke tests also check the default local socket.

Locally: `scripts/package_linux.sh --version v0.1.1 --arch x86_64 --format portable,deb`
(builds NeuroWatch's GStreamer once into `build/gstreamer-sdk`; use a sibling checkout at the
commit in `neurowatch.ref`, or set `NEUROWATCH_DIR`), and on Windows, from an MSVC
developer shell with GStreamer's MSVC packages and Inno Setup 6,
`scripts/package_windows.ps1 -Version v0.1.1` (use `-InnoSetupCompiler` for a custom compiler path).
