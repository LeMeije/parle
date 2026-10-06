# Installing Parle

No technical knowledge needed. It takes about five minutes, most of which is
the speech model downloading.

## 1. Download the right file

Go to the **[latest release](https://github.com/LeMeije/parle/releases/latest)**
and scroll to **Assets**. Download only one file:

| Your computer | The file to download |
|---|---|
| Any Mac (Apple Silicon or Intel), macOS 12 or later | the one ending in **`_universal.dmg`** |
| Windows 11 PC | the one ending in **`_x64-setup.exe`** |

Ignore **Source code (zip)** and **Source code (tar.gz)**. They are the raw
programming files, not the app, and they will not install anything.

## 2. Install it

### On a Mac

1. Open the `.dmg` you downloaded. A window appears showing the Parle icon and
   an Applications folder.
2. Drag **Parle** onto **Applications**.
3. Open your **Applications** folder and double-click **Parle**.

The first time, macOS will stop it and say it cannot check the app for
malicious software. That is normal for a free app from an independent
developer who has not paid for Apple's developer programme. To let it through:

**macOS 15 Sequoia or later**

1. Click **Done** on the warning (not "Move to Bin").
2. Open **System Settings**, then **Privacy & Security**.
3. Scroll down to the **Security** section. You will see a line saying Parle
   was blocked. Click **Open Anyway**.
4. Enter your Mac password (or use Touch ID), then click **Open Anyway** once
   more.

**macOS 12, 13 or 14**

1. In your Applications folder, hold **Control** and click **Parle** (or
   right-click it).
2. Choose **Open**, then click **Open** in the box that appears.

You only do this once. After that Parle opens normally.

**If macOS says Parle "is damaged and can't be opened"**: it is not damaged,
this is a stricter version of the same warning. Open the **Terminal** app,
paste this line, press Return, then open Parle again:

```bash
xattr -dr com.apple.quarantine /Applications/Parle.app
```

### On Windows

1. Your browser may say the file "isn't commonly downloaded". In Edge, click
   the **...** next to the download, then **Keep**, then **Show more**, then
   **Keep anyway**. In Chrome, click **Keep**.
2. Double-click the `_x64-setup.exe` file.
3. A blue box may appear saying **Windows protected your PC**. Click
   **More info**, then **Run anyway**. As on the Mac, this is because the app
   is free and unsigned, not because anything is wrong with it.
4. Click through the installer. It installs just for you, so it will not ask
   for an administrator password.

## 3. First-time setup inside Parle

Parle walks you through this itself. In short:

1. **Permissions.** On a Mac it asks for two: **Microphone** (to hear you)
   and **Accessibility** (to notice your dictation key and type the words
   where your cursor is). For Accessibility, click **Open Settings**, turn
   Parle on in the list, then come back. Parle notices by itself; if it does
   not, quit and reopen it. Windows only asks for the microphone.
2. **Model.** Parle recommends a speech model for your computer and
   downloads it once (the usual recommendation is about 200 MB). After that
   it works with no internet connection.
3. **Your key.** On a Mac it is the **🌐 Fn** key, on Windows **Left Ctrl**.
   Hold it, talk, let go, and your words appear wherever you were typing.
   Mac tip: in **System Settings > Keyboard**, set "Press 🌐 key to" to
   **Do Nothing** so the Mac's own dictation does not fight Parle for it.
4. **Try it.** Say something to check it works, then finish.

## Updating

Parle does not update itself yet. When there is a new version, download it
from the same [releases page](https://github.com/LeMeije/parle/releases/latest)
and install it over the top. Your history, settings and models are kept.

On a Mac, if your dictation key stops working after an update, open **System
Settings > Privacy & Security > Accessibility**, select Parle, remove it with
the **minus (-)** button, then add it again with **plus (+)** from your
Applications folder.

## Uninstalling

- **Mac:** quit Parle, then drag it from Applications to the Bin.
- **Windows:** **Settings > Apps > Installed apps**, find Parle, click
  **...** then **Uninstall**.

## Privacy

Your speech is turned into text on your own computer, and your history stays
there. The only internet use is downloading the speech models you choose. Two
features can move your text, and both are off until you switch them on:
Refine mode, which hands your text to an AI tool you have already installed
(see [REFINE.md](REFINE.md)), and Sync, which shares history between your own
computers on the same network.
