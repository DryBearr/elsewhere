"""Serves a playlist for the apps' UI tests, on this machine (the Android emulator reaches it
as 10.0.2.2, the iOS simulator as 127.0.0.1):

    /music-playlist.html   two <audio> tags, which yt-dlp reads as a playlist of songs
    /video-playlist.html   two <video> tags: a playlist of videos
    /media/<file>          the files in tests/media (long.m4a 6 s, song.m4a 2 s, long.mp4
                           8 s, clip.mp4 2 s; the videos are 96x64)

The apps save it through their real Add path (yt-dlp, the library) before testing the
Music tab on it.

    python3 scripts/selftest_server.py <port>
"""

import http.server
import pathlib
import sys

MEDIA = pathlib.Path(__file__).resolve().parent.parent / 'tests' / 'media'
PAGES = {
    '/music-playlist.html': b'''<html><head><title>Music Playlist</title></head><body>
<audio src="/media/long.m4a"></audio>
<audio src="/media/song.m4a"></audio>
</body></html>
''',
    '/video-playlist.html': b'''<html><head><title>Video Playlist</title></head><body>
<video src="/media/long.mp4"></video>
<video src="/media/clip.mp4"></video>
</body></html>
''',
}
TYPES = {'.m4a': 'audio/mp4', '.mp4': 'video/mp4', '.webm': 'video/webm', '.opus': 'audio/ogg'}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split('?', 1)[0]
        if path in PAGES:
            self.answer(200, 'text/html; charset=utf-8', PAGES[path])
            return
        name = path.removeprefix('/media/')
        file = MEDIA / name
        # Only plain names inside tests/media.
        if path.startswith('/media/') and '/' not in name and file.is_file():
            self.answer(200, TYPES.get(file.suffix, 'application/octet-stream'), file.read_bytes())
            return
        self.answer(404, 'text/plain', b'not found')

    def answer(self, status, kind, body):
        self.send_response(status)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == '__main__':
    port = int(sys.argv[1])
    http.server.ThreadingHTTPServer(('0.0.0.0', port), Handler).serve_forever()
