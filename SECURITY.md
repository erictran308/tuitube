# Security

## Reporting a vulnerability

Report it privately at <https://github.com/erictran308/tuitube/security/advisories/new> (Security → Report a vulnerability), not in a public issue.

Say what a video, channel, feed or file has to contain, what happens, your tuitube version (`tuitube --version`), OS and terminal.

## What counts

Anything a video, channel or feed on YouTube, or a file tuitube is pointed at, can do to you through tuitube. For example:

- a title, name, description or thumbnail that runs code, opens something without asking, or garbles the terminal;
- tuitube starting yt-dlp or mpv with options or URLs other than the ones it built;
- anything sent to YouTube that you didn't ask for, such as watch history;
- your subscriptions or history left where another account can read them.

Bugs in yt-dlp or mpv themselves belong at <https://github.com/yt-dlp/yt-dlp> and <https://github.com/mpv-player/mpv>.
