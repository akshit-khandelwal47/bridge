# Answers every request with headers for a 1 MB body, then sends one byte every 5 s: under apt's
# 15 s per-read timeout, so apt never times out. Stand-in for a connection that stalls without dying.
import socket, sys, threading, time

def serve(conn):
    try:
        conn.recv(65536)
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n"
                     b"Content-Length: 1000000\r\nConnection: close\r\n\r\n")
        while True:
            time.sleep(5)
            conn.sendall(b"x")
    except OSError:
        pass
    finally:
        conn.close()

listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 8080))
listener.listen(64)
print("drip server listening on 127.0.0.1:8080", flush=True)
while True:
    connection, _ = listener.accept()
    threading.Thread(target=serve, args=(connection,), daemon=True).start()
