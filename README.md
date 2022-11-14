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
    * Talking Points
    * The Devils and the Details (requires Audience explicitly disabled)
* The Jackbox Party Pack 8
    * Job Job (requires Audience explicitly disabled)
    * Drawful Animate
* Drawful 2 International

### Tested known non-working games:

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
* UGC (user-made episodes, etc)
* Blobcast / API v1(? what is the real name)

## Usage

*Note*: Using the server only works on the Windows version of the game. If you are on Linux, use Proton.

This is **NOT** meant to be used in any form of serious environment. This is an experimental testing server, and there is no way to safely configure a web browser to connect to this server currently.

1. `npm install ws` to install the WebSockets NodeJS module.
2. Generate a TLS certificate for the web server to use
   * You can use [this tutorial](https://gist.github.com/cecilemuller/9492b848eb8fe46d462abeb26656c4f8) to generate these certificates (make sure to use the *Chrome, IE11 & Edge* method when trusting the new certificates).
   * If you are running this on a seperate server (not localhost), you should use a domain name and Let's Encrypt to access the server.
3. If you are running this on a seperate server, edit the top of `johnbox.js` to change accessibleHost to a host accessible by all players (e.g. public IP)
   * Also, if your certificates are not named localhost.crt and localhost.key, change those also in `johnbox.js`
5. `node johnbox.js` (or `sudo node johnbox.js` on Linux) to start the server.
6. Redirect the game to connect to your server. `jbg.config.jet` in each minigame folder has a `serverUrl` parameter.
7. Go to https://jackbox.tv?server=localhost and have fun!
