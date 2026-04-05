# Black Sea

A terminal multiplayer sailing game set in the Stockholm archipelago. Navigate your boat through real GIS coastline data, explore islands, and chat with other sailors.

## Install

**macOS via Homebrew:**

```sh
brew tap dtennander/black-sea
brew install black-sea
```

**Or download a binary directly** from the [latest release](https://github.com/dtennander/black-sea/releases/latest):

| Platform              | Binary                           |
| --------------------- | -------------------------------- |
| macOS (Apple Silicon) | `black-sea-client-macos-aarch64` |
| macOS (Intel)         | `black-sea-client-macos-x86_64`  |
| Linux                 | `black-sea-client-linux`         |
| Windows               | `black-sea-client-windows.exe`   |

## Play

```sh
black-sea
```

Enter your name, then sail around using the arrow keys. You'll automatically connect to the shared server.

| Key        | Action            |
| ---------- | ----------------- |
| Arrow keys | Sail              |
| Enter      | Send chat message |
| Esc        | Quit              |

## Building from source

Requires Rust 1.85+.

```sh
git clone https://github.com/dtennander/black-sea
cd black-sea
cargo build --release --bin black-sea
```

To connect to a different server:

```sh
BLACK_SEA_SERVER=ws://localhost:7456 black-sea
```

## Running a server

```sh
cargo build --release --bin server
./target/release/server
```

The server downloads OpenStreetMap land polygons on first run and listens on port 7456. Map data is cached in `./osm-cache/`.
