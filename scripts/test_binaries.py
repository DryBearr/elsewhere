"""Copies test programs out of a `cargo test --no-run --message-format=json` log.

    python3 scripts/test_binaries.py build.json ytdlp playlists

copies the programs for tests/ytdlp.rs and tests/playlists.rs to ./ytdlp-tests and
./playlists-tests, for running on an emulator or simulator (.github/workflows/mobile.yml).
"""

import json
import shutil
import sys


def main():
    log, names = sys.argv[1], sys.argv[2:]
    if not names:
        sys.exit(__doc__)
    found = {}
    with open(log) as lines:
        for line in lines:
            message = json.loads(line)
            if message.get('reason') == 'compiler-artifact' and message.get('executable'):
                found[message['target']['name']] = message['executable']
    for name in names:
        if name not in found:
            sys.exit(f'no test program for {name} in {log}')
        shutil.copy(found[name], f'{name}-tests')
        print(f'{name}-tests <- {found[name]}')


if __name__ == '__main__':
    main()
