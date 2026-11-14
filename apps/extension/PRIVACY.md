# Whoosh browser extension — privacy policy

Effective: the first version published in the Chrome Web Store or on addons.mozilla.org. This policy covers the Whoosh extension for Chromium browsers (Chrome, Edge, Brave, Opera, Vivaldi) and Firefox. The source code is public under GPL-3.0-or-later: <https://github.com/coderzonebd/whoosh-download-manager>.

## Summary

- The extension **collects no data**. It has no analytics, no telemetry, no advertising, no accounts and no remote server.
- It talks to exactly one program: the **Whoosh app on your own computer**, through the browser's native messaging. Nothing the extension handles is sent to the developers or to any third party.
- What the extension keeps (settings, a short memory of downloads it handed over) stays in your browser's extension storage on your computer.

## What the extension reads, and why

| What | When | Where it goes |
|---|---|---|
| The address (URL) of a download, its file name, size and type | When the browser starts a download that matches your settings, or when you choose "Download with Whoosh" | To the Whoosh app on this computer, which then downloads the file |
| The cookies the browser holds for that download's address, the page that linked to it (referrer) and the browser's user-agent string | Together with the download | To the Whoosh app on this computer, so it can repeat the request exactly as the browser would. Cookies are read only for the address being downloaded |
| Response headers (type and size) of media requests | While the media detector is on | Kept in the browser's session storage for that tab only, to list the media in the toolbar popup. Cleared when the tab navigates or closes |
| The site's address (origin only, e.g. `https://example.com/`) when your pointer rests on a download link | While "prepare the connection" is on | To the Whoosh app on this computer, which opens a connection early. The link itself is not sent |
| The links on a page and their text | When you use the context menu on that page, or when self-healing reads the page a download came from (see below) | Used inside the extension to choose the right links; the chosen link goes to the Whoosh app |
| The text of a download link you click | Kept in memory for one minute | Used only to recognise the same link again if that download's address later expires |

The extension does not read page contents beyond links, does not record your browsing history, and does not see what you type.

## What the extension stores on your computer

- **Settings** (`storage.local`): your choices on the options page.
- **Downloads it handed over** (`storage.local`): for each, the download's address, the link and page it came from, the link text, file name and size, and the time. This is what makes *self-healing* possible: when a paused download's address has expired, the extension can find the fresh address on the same page. Entries are deleted when the download completes or is removed in Whoosh, after 30 days at the latest, and at most 500 are kept. Downloads from private windows are never stored.
- **Detected media** (`storage.session`): per tab, cleared with the tab and when the browser closes.

Cookies are never stored by the extension; they are read at the moment they are needed and passed to the Whoosh app.

## Self-healing

When a download's address expires, the extension may open the page that download came from in a background tab (muted, closed again afterwards) to find the fresh link, just as you would by visiting the page again. This uses your normal browser session, never fills in forms, never enters passwords and never solves CAPTCHAs. If it is unsure, it asks you in the toolbar popup. You can turn self-healing off on the options page.

## The Whoosh app

The extension does nothing on its own: it hands downloads to the Whoosh desktop app, which must be installed separately and which registers itself as the extension's native messaging host. The app runs entirely on your computer. It has its own privacy notes in its documentation; by default it sends nothing to the developers either (crash reports are opt-in and off by default).

## Permissions

The permissions the extension asks for, and what each one is used for, are listed in [`store/permissions.md`](store/permissions.md).

## Changes and contact

Changes to this policy are published with the extension's source code and listed in its changelog. Questions: open an issue at <https://github.com/coderzonebd/whoosh-download-manager/issues>.
