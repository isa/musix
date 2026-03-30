# MUSIX

A minimalist terminal-based MP3 music player built with Rust.

![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white)
![Terminal](https://img.shields.io/badge/Terminal-UI-green?style=for-the-badge)
![Music](https://img.shields.io/badge/MP3-Player-orange?style=for-the-badge)

[![asciicast](https://asciinema.org/a/730123.svg)](https://asciinema.org/a/730123)

## Features

- **Beautiful TUI**: Clean terminal interface with cyberpunk green theme
- **High-Quality Playback**: MP3 audio support with crystal-clear sound
- **Visual Progress**: Real-time progress bar with time display
- **Smart Controls**: Intuitive keyboard controls with popup help
- **Smooth Seeking**: Instant seek without playback interruption
- **Playback Modes**: Normal sequential and random shuffle
- **Keyboard-Driven**: Lightning-fast keyboard-only interface
- **Fuzzy Search**: Real-time search with `/` key - find songs instantly
- **Vim-Style Navigation**: Full vim keybinding support (hjkl, gg/G, n/N, q)
- **Pitch-Preserving Speed Control**: Speed up or slow down playback without chipmunk/horror voice effects (custom WSOLA algorithm)

## Quick Start

### Prerequisites

- **Rust 1.70+** - [Install Rust](https://rustup.rs/)
- **Audio Libraries** (Linux): `libasound2-dev pkg-config`

### Installation

```bash
# Clone the repository
git clone git@github.com:isa/musix.git
cd musix

# Build and run
cargo run

# Or build optimized release version
cargo build --release
./target/release/musix
```

### Quick Usage
1. **Start the player**: `cargo run -- /path/to/music` or just `cargo run` to use current directory
2. **Navigate**: Use `j/k` or arrow keys to browse songs
3. **Search**: Press `/` and type to find songs instantly
4. **Play**: Press `Enter` or `Space` to play selected song
5. **Speed up**: Press `+`/`-` to adjust playback speed, `0` to reset
6. **Jump**: Use `gg` (first song) or `G` (last song)
7. **Help**: Press `?` to see all controls
8. **Quit**: Press `q` or `Esc` to exit

### Music Files

MUSIX scans for MP3 files recursively:

```bash
# Play from a specific folder
musix /path/to/music

# Play from current directory (default)
musix
```

## Controls

> **Tip**: Press **?** anytime to view the interactive controls popup!

### Essential Keys

| Key | Action |
|-----|--------|
| **`Space/↵`** | **Smart Play** - Play selected song or pause current |
| **`/`** | **Search Mode** - Enter fuzzy search |
| **`?`** | **Show/Hide help popup** |
| **`q/Esc`** | **Exit** |

### Navigation & Playback

| Key | Action |
|-----|--------|
| `↑/↓` or `j/k` | Navigate songs (vim-style) |
| `Space/↵` | Play/pause (same functionality) |
| `←/→` or `h/l` | Play previous/next song |
| `gg` / `G` | Jump to first/last song |
| `,` / `.` | Seek backward/forward 5 seconds |
| `<` / `>` | Same as above |
| `+` or `=` | Increase playback speed (+0.1x) |
| `-` | Decrease playback speed (-0.1x) |
| `0` | Reset speed to 1.0x |
| `1`-`9` | Jump to 10%-90% of song |
| `r` | Toggle Random mode |

### Search Mode

| Key | Action |
|-----|--------|
| **`/`** | Enter search mode |
| `n` / `N` | Navigate to next/previous search result |
| `↑/↓` or `j/k` | Navigate through filtered results |
| `Enter` | Play selected song and exit search |
| `Esc` | Exit search mode |
| `Backspace` | Delete characters from search query |
| `Any text` | Type to search (fuzzy matching) |

## Interface

MUSIX features a clean, 4-panel interface that maximizes space for your music:

```
┌─────────────────────────────────┐
│             MUSIX               │  ← Title Bar
├─────────────────────────────────┤
│ Songs - Search: rock            │  ← Song List (Search Mode)
│ → ♪ 1. Rock Song                │    or "Songs" (Normal Mode)
│     5. Another Rock Song        │    (Scrollable, Filtered)
│     12. Rock Ballad             │
│     More filtered results...    │
├─────────────────────────────────┤
│ ████████████████░░░░ 02:30/04:15│  ← Progress Bar
├─────────────────────────────────┤
│ Search Mode | Songs: 15/120 |.. │  ← Status & Search Info
└─────────────────────────────────┘
```

### Interactive Controls Popup (Press **?**)

```
┌──────────────────────────────────┐
│            CONTROLS              │
│                                  │
│ ↑/↓ or j/k - Navigate songs     │
│ Space/↵    - Play/Pause          │
│ ←/→ or h/l - Play prev/next song│
│ gg/G       - Jump to first/last  │
│ /          - Enter search mode   │
│ n/N        - Next/prev search    │
│ ,/.        - Seek ±5 seconds     │
│ R          - Toggle random mode  │
│ +/=        - Increase speed      │
│ -          - Decrease speed      │
│ 0          - Reset speed (1.0x)  │
│ 1-9        - Jump to 10%-90%     │
│ q/Esc      - Exit application    │
│ ?          - Toggle this help    │
└──────────────────────────────────┘
```

## Smart Features

### Fuzzy Search
- **Instant Search**: Press `/` to enter search mode
- **Real-time Filtering**: Results update as you type
- **Fuzzy Matching**: Finds songs even with partial or misspelled text
- **Smart Scoring**: Prioritizes exact matches → substring matches → fuzzy matches
- **Search Navigation**: Use `n/N` to quickly jump between results
- **Quick Play**: Press Enter on any result to play immediately

**Example**: Searching "btl" will match "Battle Song", "Beautiful", "Subtitle"

### Playback Speed Control
- **Pitch-Preserving**: Speed changes use a custom WSOLA algorithm — voice sounds natural at any speed
- **Range**: 0.25x to 4.0x in 0.1x increments
- **Quick Reset**: Press `0` to instantly return to normal speed
- **Status Display**: Current speed shown in the status bar (e.g., "Speed: 1.50x")

### Visual Indicators
- **`→`** Currently selected song in the list
- **`♪`** Currently playing song indicator  
- **Progress Bar** Real-time playback progress with time
- **Search Title** Shows current search query in song list header
- **Result Count** Displays filtered results count (e.g., "15/120 songs")

### Playback Modes
- **Normal Mode**: Sequential playback through your playlist
- **Random Mode**: Intelligent shuffle (excludes current song)

### Smart Space/Enter Key
- **Initial state**: Plays the first selected song
- **Different song selected**: Plays the selected song immediately
- **Same song selected**: Toggles play/pause for current song

### Vim-Style Navigation
- **Movement**: `hjkl` for navigation (h=left, j=down, k=up, l=right)
- **Jumping**: `gg` jumps to first song, `G` jumps to last song
- **Search Navigation**: `n/N` for next/previous search results
- **Quit**: `q` as alternative to Escape

## Technical Details

### Architecture
- **Player Engine**: State management with smart playback control
- **Terminal UI**: Ratatui-powered responsive interface  
- **Audio Engine**: Rodio-based high-quality MP3 processing
- **Performance**: Efficient seeking without playback interruption

### Core Dependencies
- **`rodio`** - Professional audio playback and MP3 decoding
- **`ratatui`** - Modern terminal user interface framework
- **`crossterm`** - Cross-platform terminal control

## Development

### Project Structure

```
musix/
├── src/
│   └── main.rs          # Complete application (~1600 lines)
├── .github/workflows/   # CI/CD automation
├── Cargo.toml          # Dependencies and metadata
├── rustfmt.toml        # Code formatting rules
└── README.md           # Documentation
```

### Building & Testing

```bash
# Development build
cargo build

# Optimized release build
cargo build --release

# Run all tests
cargo test

# Code quality checks
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

## Troubleshooting

### No Music Files Found

**Issue**: `No MP3 files found`

**Solutions**:
```bash
# Specify a directory containing MP3 files
musix /path/to/your/music

# Or run from a directory that has MP3 files
cd /path/to/your/music && musix
```

### Linux Audio Issues

**Issue**: No audio output or initialization errors

**Solutions**:
```bash
# Install required audio libraries
sudo apt-get update
sudo apt-get install libasound2-dev pkg-config

# For other distributions
sudo pacman -S alsa-lib pkg-config  # Arch
sudo dnf install alsa-lib-devel pkgconf  # Fedora
```

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests if applicable
5. Submit a pull request

## Acknowledgments

- **Rodio** team for excellent Rust audio library
- **Ratatui** team for powerful TUI framework
- **Rust** community for amazing ecosystem

---

**Built with Claud Code**
