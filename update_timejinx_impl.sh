curl "https://192.168.1.10/@cdn.jackboxgames.com/time-trivia/$(basename "$1")_0.png" -o - > /dev/null
curl "https://192.168.1.10/@cdn.jackboxgames.com/time-trivia/$(basename "$1")_1.png" -o - > /dev/null

