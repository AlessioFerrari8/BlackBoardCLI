"""Scripted guest for the demo GIF: joins a local host and draws + chats over the real protocol."""
import json, socket, sys, time

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 7878

def send(s, **msg):
    s.sendall((json.dumps(msg) + "\n").encode())

for _ in range(100):
    try:
        s = socket.create_connection(("127.0.0.1", PORT))
        break
    except OSError:
        time.sleep(0.1)
else:
    sys.exit(1)

send(s, t="hello", name="friend", proto=2)
time.sleep(3)
send(s, t="chat", text="hey! let me draw something")
time.sleep(0.8)

# "HI" on the right side of the board, stroke by stroke
H = [(50, y) for y in range(4, 13)] + [(x, 8) for x in range(51, 56)] + [(56, y) for y in range(4, 13)]
I = [(x, 4) for x in range(60, 67)] + [(63, y) for y in range(5, 12)] + [(x, 12) for x in range(60, 67)]
for color, pts in ((4, H), (6, I)):
    for x, y in pts:
        send(s, t="cursor", x=x, y=y)
        send(s, t="draw", color=color, points=[[x, y], [x + 1, y]])
        time.sleep(0.07)
    time.sleep(0.3)

send(s, t="chat", text="your turn :)")
time.sleep(9)

# A few letters under the HI, one at a time like someone typing
for i, c in enumerate("cool!"):
    send(s, t = "cursor", x = 55 + 1, y = 15)
    send(s, t = "text", color = 5, x = 54 + i, y = 15, text = c)
    time.sleep(0.15)
time.sleep(30)
