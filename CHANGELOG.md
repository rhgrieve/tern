# Changes

## Unreleased

**New**

- Write a message in your own editor with Ctrl+Shift+E or the button beside the message box:
  your default app for Markdown or text, else `$VISUAL` or `$EDITOR`, in a terminal when it needs
  one. The text comes back when you close it, also if you went to another chat meanwhile. Not in
  the Flatpak.
- Messages can have italic (`*so*` or `_so_`), `~~strike~~`, headings (`#` to `###`), quotes
  (`> `), numbered lists (`1. `) and tables (`| a | b |` over a `|---|---|` line), as Teams
  writes them; `* ` starts a list item as `- ` does. Editing a sent message keeps them.
- The message box shows Markdown as it will be sent while you type: each look shows as you write
  it, and the characters that make it stay, dimmed.
- Ctrl+Shift+L opens the background service's log of the last two days in your app for text,
  where Tern was installed with its script. The log says which version runs and what happened in
  plain words, without colour codes, and web addresses in it go no further than their host.
- When Teams refuses a message, Tern says what Teams answered, as "Not sent: HTTP 400: …", and the
  log names the files that went with it and their sizes.

**Fixed**

- A message that could not be sent stays where it was written. It was kept below everything sent
  after it, and took the mark of the newest message from them, so they looked unsent.

## 0.3.0

**New**

- Calendar, with Ctrl+E or the button above the chats: today and the two weeks ahead, also offline.
- Reminders of meetings, when you set them in Teams and as they begin, with Join and their chat.
- Join opens meetings in a Chromium-based browser as a window of its own, with a profile only for
  Teams, or else in your default browser. The first one takes you through signing in there.
- `msteams:` links, and links to meetings in messages. Installing can make Tern the default for
  `msteams:` if you have not chosen an app.
- Pictures that move do so in messages, and opened, unless your desktop has animations off.
- GIFs from Giphy load while you look at the chat. They are not fetched when a message arrives.
- Notifications when somebody calls, and for missed calls. Calls are answered in Tern, with
  sound, or declined for all your devices.
- Join takes you into a meeting in Tern, with sound, muted; the bar says while you wait to be let
  in, and how many are there. The button beside Join opens it in the browser, for video and
  sharing. Meetings with a meeting ID instead of a link still open in the browser.
- A ringtone while a call rings: your sound theme's, until you answer, decline or put the
  notification away. Not in Do Not Disturb.
- Calls with sound: the button at the top of a 1:1 or group chat rings the others. A bar under it shows the
  call, with mute and hang up, in every window. Each call runs in a program of its own,
  `tern-call`, which hangs up when Tern's window goes. Not yet in the Flatpak.
- In a narrow window the call's bar is also above the list of chats.
- Chats with something written and not sent say Draft in the list, with what you wrote.

## 0.2.1

**New**

- Pinned chats, the same ones as in Teams, above the others. Pin a chat or take that back with a
  right click in the list, or with Ctrl+Shift+P for the open chat.
- A button that leads to the newest message.
- Pictures have their room before they come, and say so while they load. One that did not load is
  tried again with a click, and one on another site opens in the browser.
- Emoji of your organization's own move, if they do.

**Fixed**

- Search and pictures close with Esc, and with a click beside them.

## 0.2.0

**New**

- Pictures open on click, in full, to look at, copy or save.
- Read receipts: one check while your newest message is on its way, two when it arrived, blue when
  it was read. Point at them to see by whom.
- Other people's status on their faces.
- Notifications when somebody reacts to one of your messages.
- Link previews, and web addresses in plain text are links, sent and received.
- `:emoji:` while writing, and spell checking with your system's dictionaries.
- Emoji of your organization's own as reactions.
- Reactions show at once.

**Fixed**

- Pictures appear in an open chat as soon as they are here, also further back.
- The chat that is selected opens again from the list of a narrow window.
- Long text stayed shifted up in the message box, with an empty strip below.
- A chat moved by a few pixels when a message was read.
- One long word or a row of mentions made a whole chat wider than its window. Pictures and reactions
  fit narrow windows, too.
- "Sending…" no longer flashes by and moves the chat.
- The time stands on the line of the name, and messages no longer show a text cursor.

Spell checking needs a dictionary for your language; the Flatpak has its own.

## 0.1.0

The first release: presence that is true, chats and channels offline, sending, notifications, and the
window.
