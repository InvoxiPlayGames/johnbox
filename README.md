# Johnbox

A private server implementation in Rust for modern Jackbox Games services (Ecast / API v2).

**This project is not related to or endorsed by Jackbox Games, Inc.**

## Supported Software

### Tested known working games:

* The Jackbox Party Pack 7
    * Quiplash 3
    * Champ'd Up
    * Blather Round
    * Talking Points
    * The Devils and the Details (requires Audience explicitly disabled)
* The Jackbox Party Pack 8 (all games)
    * Job Job (requires Audience explicitly disabled)
* The Jackbox Party Pack 9 (all games)
* The Jackbox Party Pack 10 (all games)
    * FixyText (does not work, and is probably a wontfix)
* Drawful 2 International

### Tested known non-working games:

* Quiplash 2 InterLASHional
    * Quiplash 2 InterLASHional only uses API v2 for gathering server info. Rooms are still handled via blobcast.
* The Jackbox Party Pack 6
    * All games in Party Pack 6 only use API v2 for gathering server info. Rooms are still handled via blobcast.
* FixyText
    * Requires complex API for collaborative text editing, which is not implemented and difficult to reverse engineer.
* All games prior use Blobcast / API v1 (likely not the true names), which uses socketio for WebSockets and is currently not supported. 

## Unimplemented features

* Object security
* Room passcodes
* Audiences
* Moderation features
* UGC (user-made episodes, etc)
* Blobcast / API v1(? what is the real name)

## Usage

### Running

Building and running is relatively simple. Just install Rust and run

```shell
cargo run --release
```

Or look in releases for a binary.

## Caching games

There are shell scripts in this project that serve to cache game assets in the server's cache that can't be retrieved in one playthrough.

You can run each script with your `steam/steamapps/common` directory, and the server's accessible host (HTTPS required) as arguments

```shell
sh update_dodoremi.sh "$HOME/.steam/steam/steamapps/common" "192.168.1.10"
```

- `update_dodoremi.sh`
    - Dependencies:
        - find
        - jq
        - parallel
        - curl
    - Caches the sounds for every instrument that can be loaded in the game
- `update_junktopia.sh`
    - Dependencies:
        - find
        - parallel
        - curl
    - Caches every item that can be bought in the game
- `update_timejinx.sh`
    - Dependencies:
        - find
        - parallel
        - curl
    - Caches every impostor image in the game
