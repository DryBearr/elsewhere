"""Bridge between Rust (src/ytdlp/python.rs) and yt-dlp.

Rust calls only setup(), version(), info() and download(). Everything yt-dlp prints,
logs or reports goes through the `emit` callback, so nothing touches stdout, stderr
or stdin. Errors leave this module as one of three exceptions: BadArgs, Cancelled
or Failure.
"""

import builtins
import getpass
import io
import json
import optparse
import os
import re
import sys

yt_dlp = None
DownloadCancelled = None
DownloadError = None

# Flags that only yt-dlp's command-line program acts on. parse_options() accepts them,
# but the library would silently ignore them, so they are rejected instead.
CLI_ONLY = {
    'update_self': '-U/--update/--update-to',
    'rm_cachedir': '--rm-cache-dir',
    'load_info_filename': '--load-info-json',
    'list_extractors': '--list-extractors',
    'list_extractor_descriptions': '--extractor-descriptions',
    'list_impersonate_targets': '--list-impersonate-targets',
    'ap_list_mso': '--ap-list-mso',
}

class BadArgs(Exception):
    """The flags cannot be used. Becomes Error::BadArgs."""


class Failure(Exception):
    """yt-dlp failed. args are (kind, message). Becomes Error::YtDlp."""


class Cancelled(Exception):
    """Replaced in setup() by a subclass of yt-dlp's DownloadCancelled. Becomes Error::Cancelled."""


def setup(paths, run_js):
    """Put `paths` first on sys.path, block typed input, import yt-dlp and give it the
    built-in JavaScript engine `run_js(code) -> printed text`. Returns yt-dlp's version."""
    global yt_dlp, DownloadCancelled, DownloadError, Cancelled

    for path in reversed([os.fspath(path) for path in paths]):
        while path in sys.path:
            sys.path.remove(path)
        sys.path.insert(0, path)

    # Apps have no console. Fail fast instead of waiting forever for typed input.
    builtins.input = _no_input
    getpass.getpass = _no_input
    sys.stdin = io.StringIO()

    # iOS apps cannot start processes. Python (3.13+) already makes subprocess.Popen raise
    # OSError there, so yt-dlp sees ffmpeg and JS runtimes as unavailable. Do not replace
    # Popen: yt-dlp subclasses it.

    import yt_dlp as module
    import yt_dlp.options
    from yt_dlp.utils import DownloadCancelled as cancelled_base, DownloadError as download_error

    # --help and --version print to stdout and exit the process. Refuse them instead.
    # (Swapping sys.stdout to catch the text is not an option: it is shared by all threads.)
    parser = getattr(yt_dlp.options, '_YoutubeDLOptionParser', optparse.OptionParser)
    parser.print_help = _command_line_only('--help')
    parser.print_version = _command_line_only('--version')

    class _Cancelled(cancelled_base):
        msg = 'Cancelled'

    _add_js_engine(run_js)
    yt_dlp, DownloadCancelled, DownloadError, Cancelled = module, cancelled_base, download_error, _Cancelled
    return version()


def _add_js_engine(run_js):
    """Lets yt-dlp solve YouTube's JavaScript challenges with the engine built into the app.

    yt-dlp's own solvers start Deno, Node or QuickJS as a separate program, which iOS
    forbids. This one gives the same solver script (from the yt-dlp-ejs package) to
    `run_js` instead, so YouTube works the same on every platform.
    """
    try:
        from yt_dlp.extractor.youtube.jsc._builtin.ejs import EJSBaseJCP
        from yt_dlp.extractor.youtube.jsc.provider import (
            JsChallengeProviderError,
            register_preference,
            register_provider,
        )
    except ImportError:
        return  # a yt-dlp without challenge providers; nothing to add

    class ElsewhereJCP(EJSBaseJCP):
        PROVIDER_NAME = 'elsewhere'
        JS_RUNTIME_NAME = 'built-in QuickJS'
        PROVIDER_VERSION = '1.0.0'
        BUG_REPORT_LOCATION = 'https://github.com/DryBearr/elsewhere/issues'

        def is_available(self):
            # Always there; becomes False only when no solver script can be found.
            return self._available

        def _run_js_runtime(self, stdin, /):
            try:
                return run_js(stdin)
            except RuntimeError as err:
                raise JsChallengeProviderError(f'JavaScript error: {err}') from None

    try:
        register_provider(ElsewhereJCP)
    except AssertionError:
        return  # already registered by an earlier setup()

    @register_preference(ElsewhereJCP)
    def _prefer_built_in(provider, requests):
        return 1000  # above every external runtime, so all platforms behave the same


def version():
    return yt_dlp.version.__version__


def info(url, args, emit, cancelled):
    """Extract information without downloading. Returns ([], JSON text)."""
    return _run(url, args, emit, cancelled, download=False)


def download(url, args, emit, cancelled):
    """Download. Returns (final file paths, JSON text or None)."""
    return _run(url, args, emit, cancelled, download=True)


def _run(url, args, emit, cancelled, download):
    _check(cancelled)
    opts = _options(args, download)
    files = []
    output = _Output(emit)
    logger = _Logger(emit, cancelled)
    opts.update({
        'logger': logger,
        'progress_hooks': [_progress_hook(emit, cancelled)],
        'postprocessor_hooks': [_postprocessor_hook(emit, cancelled)],
        'post_hooks': [files.append],
        'noprogress': True,
        'consoletitle': False,
        'color': 'no_color',
    })

    result = extracted = None
    try:
        with yt_dlp.YoutubeDL(opts) as ydl:
            out_files = getattr(ydl, '_out_files', None)
            if out_files is not None:
                # Text for stdout (--print, -j, -F) becomes Output events. No console title.
                out_files.out = out_files.screen = out_files.error = output
                out_files.console = None
            extracted = ydl.extract_info(url, download=download)
            if extracted is not None:
                result = json.dumps(ydl.sanitize_info(extracted), ensure_ascii=False)
                if download and not files and not opts.get('simulate'):
                    files = _existing_files(extracted)
    except (BadArgs, Cancelled):
        raise
    except DownloadCancelled:
        # yt-dlp stopped on purpose (--max-downloads, --break-on-existing, ...). Not an error.
        pass
    except DownloadError as err:
        root = err.exc_info[1] if err.exc_info and err.exc_info[1] is not None else err
        if isinstance(root, (BadArgs, Cancelled)):
            raise root from None
        _check(cancelled)  # an extractor may have turned our Cancelled into its own error
        raise Failure(type(root).__name__, _clean(str(err))) from None
    except Exception as err:
        _check(cancelled)
        raise Failure(type(err).__name__, _clean(str(err))) from None
    finally:
        output.close()

    # By default yt-dlp logs download errors instead of raising them, so it can carry on
    # with the rest of a playlist. Nothing came out, so the last logged error is the result.
    if logger.errors and (extracted is None or (download and not files and not opts.get('simulate'))):
        _check(cancelled)
        raise Failure(*logger.errors[-1])

    files = [os.fspath(path) for path in _unique(files)]
    if not download:
        return files, result or 'null'
    return files, result


def _options(args, download):
    try:
        parsed = yt_dlp.parse_options(list(args))
    except optparse.OptParseError as err:
        raise BadArgs(_last_line(str(err)) or 'yt-dlp rejected the flags') from None
    except SystemExit as exit:
        raise BadArgs(f'yt-dlp rejected the flags (exit code {exit.code})') from None

    options = parsed.options
    for attr, flag in CLI_ONLY.items():
        if getattr(options, attr, None):
            raise BadArgs(f'{flag} only works in the yt-dlp command-line program')
    if getattr(options, 'plugin_dirs', ['default']) != ['default']:
        raise BadArgs('--plugin-dirs and --no-plugin-dirs only work in the yt-dlp command-line program')
    if parsed.urls:
        raise BadArgs('pass the URL as the url argument, not inside args')

    opts = parsed.ydl_opts
    if download:
        template = (opts.get('outtmpl') or {}).get('default') or ''
        home = (opts.get('paths') or {}).get('home') or ''
        if template == '-':
            raise BadArgs('-o - writes the media to stdout, which apps do not have; give a file name template')
        if not os.path.isabs(os.path.expanduser(home)) and not os.path.isabs(os.path.expanduser(template)):
            raise BadArgs('give an absolute output folder with -P <folder> (or an absolute -o template)')
    elif str(opts.get('extract_flat')).startswith('discard'):
        # The command-line program drops playlist entries it has already printed. info() returns them.
        opts['extract_flat'] = False
    return opts


def _last_line(text):
    """The useful part of an option parser message: its last line, without 'prog: error: '."""
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return re.sub(r'^.*?error: ', '', lines[-1]) if lines else ''


def _command_line_only(flag):
    def refuse(*args, **kwargs):
        raise BadArgs(f'{flag} only works in the yt-dlp command-line program')
    return refuse


def _no_input(prompt=''):
    raise BadArgs(f'yt-dlp asked for typed input ({str(prompt).strip() or "no prompt"}); pass the value as a flag instead')


def _check(cancelled):
    if cancelled():
        raise Cancelled()


class _Logger:
    """yt-dlp's logger interface. Also a cancel checkpoint while information is extracted."""

    def __init__(self, emit, cancelled):
        self.emit, self.cancelled = emit, cancelled
        self.errors = []  # (kind, message) of every error yt-dlp reported

    def debug(self, message):
        if message.startswith('[debug] '):
            self.emit('log', 'debug', message[len('[debug] '):])
        else:
            self.emit('log', 'info', message)
        _check(self.cancelled)

    def info(self, message):
        self.emit('log', 'info', message)
        _check(self.cancelled)

    def warning(self, message):
        self.emit('log', 'warning', _clean(message))

    def error(self, message):
        # yt-dlp logs errors from inside the `except` that caught them, so the cause is here.
        cause = sys.exc_info()[1]
        kind = type(cause).__name__ if cause is not None else 'DownloadError'
        self.errors.append((kind, _clean(message)))
        self.emit('log', 'error', _clean(message))


class _Output:
    """File-like object for yt-dlp's stdout text. Emits one Output event per line."""

    def __init__(self, emit):
        self.emit, self.pending = emit, ''

    def write(self, text):
        self.pending += text
        *lines, self.pending = self.pending.split('\n')
        for line in lines:
            self.emit('output', line.rstrip('\r'))
        return len(text)

    def flush(self):
        pass

    def isatty(self):
        return False

    def close(self):
        if self.pending:
            self.emit('output', self.pending.rstrip('\r'))
            self.pending = ''


def _progress_hook(emit, cancelled):
    def hook(status):
        state = status.get('status')
        file = status.get('filename') or status.get('tmpfilename') or ''
        if state == 'downloading':
            emit(
                'progress', file,
                _int(status.get('downloaded_bytes')) or 0,
                _int(status.get('total_bytes') or status.get('total_bytes_estimate')),
                _float(status.get('speed')),
                _int(status.get('eta')),
                _int(status.get('fragment_index')),
                _int(status.get('fragment_count')),
            )
        elif state == 'finished':
            emit('downloaded', file)
        _check(cancelled)
    return hook


def _postprocessor_hook(emit, cancelled):
    def hook(status):
        emit('postprocess', str(status.get('postprocessor') or ''), str(status.get('status') or ''))
        _check(cancelled)
    return hook


def _existing_files(extracted):
    """Final paths yt-dlp recorded, for runs where post_hooks did not fire."""
    paths = []
    for entry in [extracted, *(extracted.get('entries') or [])]:
        if not isinstance(entry, dict):
            continue
        for item in entry.get('requested_downloads') or []:
            if item.get('filepath'):
                paths.append(item['filepath'])
        if entry.get('filepath'):
            paths.append(entry['filepath'])
    return [path for path in paths if os.path.exists(path)]


def _unique(items):
    seen = set()
    return [item for item in items if not (item in seen or seen.add(item))]


def _clean(message):
    return re.sub(r'^(ERROR|WARNING): ', '', str(message)).strip()


def _int(value):
    try:
        return None if value is None else max(0, int(value))
    except (TypeError, ValueError, OverflowError):
        return None


def _float(value):
    try:
        return None if value is None else float(value)
    except (TypeError, ValueError, OverflowError):
        return None
