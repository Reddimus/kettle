# Screenshots

`kettle-hero.png` captures the native macOS 5.5.1 app. Terminal output is real;
test counts depend on the checkout. The diagram is [terminal-pipeline.svg](terminal-pipeline.svg).

To reproduce the layout in the installed app, save this as `capture.ini` and
launch `kettle --config "$PWD/capture.ini"`:

```ini
theme = TokyoNight Night
background-opacity = 1.0
media-open-mode = lane
preview-lane-side = right
agent-display = true
```

Run `kettle show docs/images/terminal-pipeline.svg --title Pipeline` from the
repository, then open the media shelf from the command palette. Capture the
native window, including its titlebar, with the OS screenshot tool.
`--screenshot` renders a synthetic test fixture and does not capture native chrome.
