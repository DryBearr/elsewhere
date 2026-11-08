"""Checks the lines the app's SelfTest writes (Android logcat, iOS console):

    ElsewhereJobs: STATE id=3 rev=7 status=RUNNING done=2 failed=0 total=20
    ElsewhereJobs: FINAL id=3 status=DONE done=19 failed=1 log=4 match=true
    ElsewhereJobs: YTDLP 2026.10.26
    ElsewhereJobs: BROKEN pid=4120
    ElsewhereJobs: RESTARTED pid=4188
    ElsewhereJobs: PLAYED file=song.m4a status=ENDED pos=2000 dur=2000 problem=null
    ElsewhereJobs: PLAYER refused=INPUT

Usage (checks are case-insensitive about the status):

    check_selftest.py <log> running <at least done>
    check_selftest.py <log> interrupted
    check_selftest.py <log> final <status> [<done> <failed>]
    check_selftest.py <log> ytdlp
    check_selftest.py <log> restarted
    check_selftest.py <log> player <file>...
    check_selftest.py <log> revisions

Exits 0 when the check holds, 1 when it does not hold (yet; the scripts keep waiting),
2 on bad usage and 3 when something is definitely wrong (a final state that does not
match the core, a revision going back, an error line): the scripts stop at once.
"""

import re
import sys

FINISHED = {'done', 'failed', 'cancelled'}


def read(path):
    states, finals, versions, errors, pids = [], [], [], [], {}
    played, refused = {}, []
    with open(path, errors='replace') as log:
        for line in log:
            if 'ElsewhereJobs' not in line:
                continue
            fields = dict(re.findall(r'(\w+)=(\S+)', line))
            if ' STATE ' in line:
                states.append(fields)
            elif ' FINAL ' in line:
                finals.append(fields)
            elif ' YTDLP ' in line:
                versions.append(line.split(' YTDLP ', 1)[1].strip())
            elif ' BROKEN ' in line or ' RESTARTED ' in line:
                pids['broken' if ' BROKEN ' in line else 'restarted'] = fields.get('pid')
            elif ' PLAYED ' in line:
                played[fields.get('file')] = fields
            elif ' PLAYER refused=' in line:
                refused.append(fields.get('refused'))
            elif ' ERROR ' in line:
                errors.append(line.strip())
    return states, finals, versions, errors, pids, played, refused


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    path, check, args = sys.argv[1], sys.argv[2], sys.argv[3:]
    states, finals, versions, errors, pids, played, refused = read(path)
    if errors:
        print('\n'.join(errors))
        return 3
    for final in finals:
        if final.get('match') != 'true':
            print(f'the screen does not match the core: {final}')
            return 3

    if check == 'running':
        least = int(args[0])
        return 0 if any(s['status'].lower() == 'running' and int(s['done']) >= least for s in states) else 1
    if check == 'interrupted':
        return 0 if any(s['status'].lower() == 'interrupted' for s in states) else 1
    if check == 'final':
        wanted = args[0].lower()
        for final in finals:
            if final['status'].lower() != wanted:
                continue
            if args[1:] and (final['done'], final['failed']) != (args[1], args[2]):
                print(f'final counts differ: {final}, wanted done={args[1]} failed={args[2]}')
                return 3
            return 0
        return 1
    if check == 'player':
        # Every file played to its end, and a file outside the media folder was refused.
        for name in args:
            state = played.get(name)
            if state and state.get('status', '').lower() != 'ended':
                print(f'{name} did not play to its end: {state}')
                return 3
            # The test files are 2 s long: ending much sooner means nothing really played
            # (for example the sound output failed and mpv skipped the audio).
            if state and int(state.get('took', '0')) < 1500:
                print(f'{name} ended after {state.get("took")} ms, not in real time: {state}')
                return 3
        if refused and refused[-1] != 'INPUT':
            print(f'a file outside the media folder was not refused as input: {refused[-1]}')
            return 3
        return 0 if all(name in played for name in args) and refused else 1
    if check == 'restarted':
        # A new process came up after the broken one.
        broken, restarted = pids.get('broken'), pids.get('restarted')
        return 0 if broken and restarted and broken != restarted else 1
    if check == 'ytdlp':
        return 0 if any('.' in version for version in versions) else 1
    if check == 'revisions':
        last, ends = {}, {}
        for state in states:
            job, revision = state['id'], int(state['rev'])
            if revision <= last.get(job, 0):
                print(f'job {job} went back from revision {last[job]} to {revision}')
                return 3
            last[job] = revision
            if state['status'].lower() in FINISHED:
                ends[job] = ends.get(job, 0) + 1
        twice = [job for job, count in ends.items() if count > 1]
        if twice:
            print(f'jobs that ended more than once: {twice}')
            return 3
        return 0
    print(__doc__)
    return 2


if __name__ == '__main__':
    sys.exit(main())
