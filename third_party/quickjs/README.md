# QuickJS-ng

The JavaScript engine yt-dlp uses to solve YouTube's challenges (src/ytdlp/js.rs). It is
built into the app, so it works on iOS too, where apps cannot start other programs.

- Version: QuickJS-ng v0.18.0, from `quickjs-amalgam.zip` on
  https://github.com/quickjs-ng/quickjs/releases/tag/v0.18.0
  (zip SHA-256 `6c05066a6b62b483e2ba0f83d7a16688c6958182b214bfd693529434cef84a9e`)
- Files: `quickjs-amalgam.c` and `quickjs.h`, unchanged. License: MIT (LICENSE).
- Built by build.rs with the `ytdlp` feature only.

To update, replace both files with the ones from a newer release and update this note.
