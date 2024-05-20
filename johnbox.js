/*
    Johnbox - Jackbox Games Private Server Implementation
    Copyright (C) 2022 InvoxiPlayGames

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU Affero General Public License as published
    by the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU Affero General Public License for more details.

    You should have received a copy of the GNU Affero General Public License
    along with this program.  If not, see <https://www.gnu.org/licenses/>.
*/

var fs = require("fs");
var https = require("https");
var wslib = require("ws");
const util = require('util')
var URL = require("url");

// the host that the custom server is accessible with
var accessibleHost = "192.168.1.44";
// the TLS certificate / key used for the server
// must have valid SNI for accessibleHost
var tlsCertificate = "certs/192.168.1.44.pem";
var tlsKey = "certs/192.168.1.44-key.pem";
// the room code to automatically assign to rooms
var roomCode = "JOHN";

var appConfigResponse = {
    "ok": true,
    "body": {
        "settings": {
            "serverUrl": accessibleHost
        }
    }
}
var roomsResponse = {
    "ok": true,
    "body": {
        "host": accessibleHost,
        "code": roomCode,
        "token": "000000000000000000000000"
    }
}
var appTag = "";
var appID = "";
var roomLocked = false;
var maxPlayers = 0;
var joinRoomsResponse = {
    "ok": true,
    "body": {
        "appId": "",
        "appTag": "",
        "audienceEnabled": false,
        "code": roomCode,
        "host": accessibleHost,
        "audienceHost": accessibleHost,
        "locked": false,
        "full": false,
        "moderationEnabled": false,
        "passwordRequired": false,
        "twitchLocked": false,
        "locale": "en",
        "keepalive": false
    }
}
var genericResponse = {
    "ok": true,
    "body": {}
}
var oldRoomResponse = {"create":true,"server":accessibleHost}

function httpHandle(req, res) {
    console.log('HTTP: ' + req.url);
    // Ecast room create
    if (req.method == 'POST' && req.url == '/api/v2/rooms') {
        var data = "";
        req.on('data', (chunk) => { data+=chunk; });
        req.on('end', () => {
            data = JSON.parse(data);
            // console.log(data);
            appID = data.appId;
            appTag = data.appTag;
            maxPlayers = data.maxPlayers;
            res.writeHead(200, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify(roomsResponse));
        });
    // Ecast app configurations
    } else if (req.url.startsWith('/api/v2/app-configs/')) {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(appConfigResponse));
    // Ecast room join
    } else if (req.url.startsWith('/api/v2/rooms/')) {
        if (appID == "") {
            // if we don't have an app ID, return 404
            res.writeHead(404, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
            res.end();
            return;
        }
        res.writeHead(200, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
        joinRoomsResponse.body.appTag = appTag;
        joinRoomsResponse.body.appId = appID;
        joinRoomsResponse.body.locked = roomLocked;
        joinRoomsResponse.body.full = playerCount >= maxPlayers;
        res.end(JSON.stringify(joinRoomsResponse));
    // Unknown
    } else {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(genericResponse));
    }
}

const server = https.createServer({
    cert: fs.readFileSync(tlsCertificate),
    key: fs.readFileSync(tlsKey)
}, httpHandle);

const wss = new wslib.WebSocketServer({ server });

wss.on('connection', function connection(ws, request) {
    console.log('WS: ' + request.url);
    if (appID == "") {
        ws.terminate();
        return;
    }
    if (request.url.includes("role=host")) HostWSHandler(ws);
    if (request.url.includes("role=player")) GuestWSHandler(ws, request.url);
});

server.listen(443, "0.0.0.0");

var roomObjects = {};
var roomVersions = {};
var roomRestrictions = {};
var roomAcls = {};
var roomTypes = {};
var roomLocks = {};
var presenceMap = {};
var playerCount = 0;
var hostPC = 2;
var guestPC = [];

function HostWSHandler(ws) {
    // reset state, start new game
    playerCount = 0;
    presenceMap = {};
    roomObjects = {};
    roomVersions = {};
    roomRestrictions = {};
    roomTypes = {};
    roomLocks = {};
    guestPC = [];
    roomLocked = false;
    presenceMap[1] = { socket: ws, id: "1", roles: { host: {} } };
    ws.on('message', function message(data) {
        var parsed = JSON.parse(data);
        // console.log(util.inspect(parsed, false, null, true));
        var text = false;
        var number = false;
        switch (parsed.opcode) {
            case "text/create":
            case "text/update":
            case "text/set":
                text = true;
            case "number/create":
            case "number/update":
            case "number/set":
                number = true;
            case "object/create":
            case "object/update":
            case "object/set": // setting / updating objects to be passed around to players
                roomObjects[parsed.params.key] = parsed.params.val;
                // if (roomLocks[parsed.params.key] === true) {
                //     return;
                // }
                roomLocks[parsed.params.key] = false;
                if (roomVersions[parsed.params.key] !== undefined) {
                    roomVersions[parsed.params.key] += 1;
                } else {
                    roomVersions[parsed.params.key] = 0;
                }
                if (parsed.params.acl) {
                    roomAcls[parsed.params.key] = parsed.params.acl[0].split(' ');
                }
                console.log("Host modified object", parsed.params.key);
                ws.send(JSON.stringify({ "pc": ++hostPC, "re": parsed.seq, "opcode": "ok", "result": {} }));
                let opcode = "object";
                if (text) {
                    opcode = "text";
                } else if (number) {
                    opcode = "number";
                    roomRestrictions[parsed.params.key] = {};
                    if (parsed.params.min)
                        roomRestrictions[parsed.params.key]["min"] = parsed.params.min;
                    if (parsed.params.type)
                        roomRestrictions[parsed.params.key]["type"] = parsed.params.type;
                    if (parsed.params.increment)
                        roomRestrictions[parsed.params.key]["increment"] = parsed.params.increment;
                }
                roomTypes[parsed.params.key] = opcode;
                for(var i = 2; i <= playerCount + 1; i++) {
                    // only deliver to clients if the acl allows it
                    if (roomAcls[parsed.params.key] && (roomAcls[parsed.params.key][1] != `id:${i}` && roomAcls[parsed.params.key][1] != '*')) continue;
                    let result = parsed.params;
                    delete result["min"];
                    delete result["type"];
                    delete result["increment"];
                    delete result["accept"];
                    delete result["reject"];
                    if (number && !text) {
                        result["restrictions"] = roomRestrictions[parsed.params.key];
                    }
                    result["version"] = roomVersions[parsed.params.key];
                    result["from"] = 1;
                    result = { "pc": ++guestPC[i - 2], "opcode": opcode, "result": result };
                    // console.log(util.inspect(result, false, null, true));
                    presenceMap[i].socket.send(JSON.stringify(result));
                }
                break;
            case "drop": // delete the room object
                delete roomObjects[parsed.params.key];
                delete roomVersions[parsed.params.key];
                delete roomAcls[parsed.params.key];
                console.log("Host dropped object", parsed.params.key);
                ws.send(JSON.stringify({ "pc": ++hostPC, "re": parsed.seq, "opcode": "ok", "result": {} }));
                break;
            case "room/get-audience": // we don't support audiences, but just return 0
                ws.send(JSON.stringify({ "pc": ++hostPC, "re": parsed.seq, "opcode": "room/get-audience", "result": {"connections": 0} }));
                break;
            case "room/lock":
                roomLocked = true;
                console.log("Host locked room");
                ws.send(JSON.stringify({ "pc": ++hostPC, "re": parsed.seq, "opcode": "ok", "result": {} }));
                break;
            case "room/exit": // kick all players
                console.log("Host closed room");
                appID = ""; // set app ID to nothing so rooms request fails
                for(var i = 2; i <= playerCount + 1; i++) {
                    presenceMap[i].socket.terminate(); // disconnect all players
                    presenceMap[i] = {};
                }
                ws.send(JSON.stringify({ "pc": ++hostPC, "re": parsed.seq, "opcode": "ok", "result": {} }));
                ws.terminate();
                break;
            case "text/filter": // Passes text to ecast to check for banned content, just looks for a default OK in response, TODO?
            case "room/start-audience": // start audience?, TODO
            default:
                // just ignore and return default OK
                console.log("Unimplemented host opcode", parsed.opcode, "with data", util.inspect(parsed, false, null, true));
                let message = { "pc": ++hostPC, "re": parsed.seq, "opcode": "ok", "result": {} };
                // console.log(message)
                ws.send(JSON.stringify(message));
                break;
        }
    });
    ws.on('close', function close() {
        appID = ""; // set app ID to nothing so rooms request fails
        for(var i = 2; i <= playerCount + 1; i++) {
            presenceMap[i].socket.terminate(); // disconnect all players
            presenceMap[i] = {};
        }
    });
    // send welcome message
    console.log("Sending host welcome.");
    ws.send(JSON.stringify({
        "pc": ++hostPC,
        "opcode": "client/welcome",
        "result": {
            "id": 1,
            "secret": "000000000000000000000000",
            "reconnect": false,
            "deviceId": "0000000000.0000000000000000000000",
            "entities": {},
            "here": {},
            "profile": null
        }
    }));
}

function GuestWSHandler(ws, url) {
    if (appID == "" || roomLocked || playerCount >= maxPlayers) {
        console.log("Client tried to connect while no room, locked or full.");
        ws.terminate();
        return;
    }
    guestPC.push(2);
    var playerID = playerCount;
    var username = URL.parse(url, true).query["name"] ? URL.parse(url, true).query["name"] : `JOHN ${playerID + 1}`;
    playerCount++;
    presenceMap[1 + playerCount] = { socket: ws, id: `${1+playerCount}`, roles: { player: { name: username } } };
    ws.on('message', function message(data) {
        var parsed = JSON.parse(data);
        var text = false;
        var number = false;
        switch (parsed.opcode) {
            case "text/update":
            case "text/set":
                text = true;
            case "number/update":
            case "number/set":
                number = true;
            case "object/update":
            case "object/set": // setting / updating objects to be passed around to players
                // TODO: implement permissions checks
                if (roomVersions[parsed.params.key] >= 0) { // only update object if it exists
                    if (roomLocks[parsed.params.key] === true) {
                        return;
                    }
                    roomLocks[parsed.params.key] = false;
                    // verify that the ACL allows the object to be edited
                    // if (roomAcls[parsed.params.key] && (roomAcls[parsed.params.key][1] != `id:${2 + playerID}` && roomAcls[parsed.params.key][1] != '*')) return;
                    roomObjects[parsed.params.key] = parsed.params.val;
                    console.log("Client", 2 + playerID, "modified object", parsed.params.key);
                    roomVersions[parsed.params.key] += 1;
                    let result = parsed.params;
                    result["version"] = roomVersions[parsed.params.key];
                    result["from"] = 2 + playerID;
                    let opcode = "object";
                    if (text) {
                        opcode = "text";
                    } else if (number) {
                        opcode = "number";
                    }
                    let message = { "pc": ++hostPC, "opcode": opcode, "result": result };
                    presenceMap[1].socket.send(JSON.stringify(message));
                }
                ws.send(JSON.stringify({ "pc": ++guestPC[playerID], "re": parsed.seq, "opcode": "ok", "result": {} }));
                break;
            case "lock":
                console.log("Client", 2 + playerID, "locked object", parsed.params.key);
                // console.log(util.inspect(parsed, false, null, true));
                roomLocks[parsed.params.key] = true;
                let message = { "pc": ++guestPC[playerID], "re": parsed.seq, "opcode": "ok", "result": {} }
                // console.log(util.inspect(message, false, null, true));
                ws.send(JSON.stringify(message));
                message = { "pc": ++hostPC, "opcode": "lock", "result": { "key": parsed.params.key, "from": 2 + playerID } };
                // console.log(util.inspect(message, false, null, true));
                presenceMap[1].socket.send(JSON.stringify(message));
                break;
            case "client/send":
                console.log("Client", parsed.params.from, "sent data to", parsed.params.to, ":", parsed.params.body);
                // send it to host, TODO: check how this is supposed to behave
                presenceMap[1].socket.send(JSON.stringify({ "pc": ++hostPC, "opcode": "client/send", "result": parsed.params }));
                ws.send(JSON.stringify({ "pc": ++guestPC[playerID], "re": parsed.seq, "opcode": "ok", "result": {} }));
                break;
            default:
                // just ignore and return default OK
                console.log("Unimplemented client opcode", parsed.opcode, "with data", parsed);
                ws.send(JSON.stringify({ "pc": ++guestPC[playerID], "re": parsed.seq, "opcode": "ok", "result": {} }));
                break;
        }
    });
    // send welcome message
    console.log("Sending client welcome.");
    
    var clientWelcome = {
        "pc": ++guestPC[playerID],
        "opcode": "client/welcome",
        "result": {
                "id": 1 + playerCount,
                "name": username,
                "secret": "00000000-0000-0000-0000-000000000000",
                "reconnect": false,
                "deviceId": "0000000000.0000000000000000000000",
                "entities": {},
                "here": {},
                "profile":{
                    "id": 1 + playerCount,
                    "roles": {
                        "player": { "name": username }
                    }
                }
        }
    };
    for(var i = 1; i <= playerCount; i++) {
        clientWelcome.result.here[`${i}`] = {};
        Object.assign(clientWelcome.result.here[`${i}`], presenceMap[i]);
        delete clientWelcome.result.here[`${i}`].socket;
    }
    var objectKeys = Object.keys(roomObjects);
    for(var i = 0; i < objectKeys.length; i++) {
        // only deliver to clients if the acl allows it
        if (roomAcls[objectKeys[i]] && (roomAcls[objectKeys[i]][1] != `id:${1 + playerCount}` && roomAcls[objectKeys[i]][1] != '*')) continue;
        clientWelcome.result.entities[objectKeys[i]] = [ roomTypes[objectKeys[i]], { key: objectKeys[i], val: roomObjects[objectKeys[i]], version: roomVersions[objectKeys[i]], from: 1 }, { "locked": roomLocks[objectKeys[i]] } ];
    }
    // console.log(util.inspect(clientWelcome, false, null, true));
    ws.send(JSON.stringify(clientWelcome));
    // send client connected message
    var clientConnected = {
        "pc": ++hostPC,
        "opcode": "client/connected",
        "result": {
            "id": 1 + playerCount,
            "userId": `00000000-0000-0000-0000-00000000000${playerCount}`,
            "name": username,
            "role": "player",
            "reconnect": false,
            "profile": {
                "id": 1 + playerCount,
                "roles": {
                    "player": { "name": username }
                }
            }
        }
    };
    presenceMap[1].socket.send(JSON.stringify(clientConnected));
}
