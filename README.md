# Johnbox

A private server implementation in NodeJS for modern Jackbox Games services (Ecast / API v2).

Currently, this server only supports hosting 1 room at a time.

**This project is not related to or endorsed by Jackbox Games, Inc.**

## Supported Software

### Tested known working games:

* The Jackbox Party Pack 7
    * Quiplash 3
    * Champ'd Up
    * Blather Round
* Drawful 2 International

### Tested known non-working games:

* The Jackbox Party Pack 8
    * Job Job (UI on the frontend is bugged, can not progress)
* Quiplash 2 InterLASHional
    * Quiplash 2 InterLASHional only uses API v2 for gathering server info. Rooms are still handled via blobcast.
* The Jackbox Party Pack 6
    * All games in Party Pack 6 only use API v2 for gathering server info. Rooms are still handled via blobcast.
* All games prior use Blobcast / API v1 (likely not the true names), which uses socketio for WebSockets and is currently not supported. 

## Unimplemented features

* Object security
* Multiple rooms
* Client reconnection
* Room passcodes
* Audiences
* Moderation features
* Blobcast / API v1(? what is the real name)

## Usage

This is **NOT** meant to be used in any form of serious environment. This is an experimental testing server, and there is no way to safely configure a web browser to connect to this server currently.

1. `npm install ws` to install the WebSockets NodeJS module.
2. Generate a TLS certificate for the web server to use.
3. Edit the top of `johnbox.js` to change accessibleHost to a host accessible by all players (e.g. public IP)
    * This must be
4. `node johnbox` to start the server.
5. Redirect the game to connect to your server. `jbg.config.jet` in each minigame folder has a `serverUrl` parameter.