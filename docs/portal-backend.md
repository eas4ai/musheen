# Musheen as the file chooser for other apps

Musheen can serve the desktop's FileChooser portal (SYS-027). Apps that open
and save files through xdg-desktop-portal, such as Flatpak apps, Firefox and
many GTK and Qt apps, then show Musheen's chooser window.

It is off by default. Two steps turn it on:

1. In Musheen, open Settings, then Integrations, and set **Desktop portal
   backend** to **Musheen**.
2. Tell xdg-desktop-portal to use Musheen for the file chooser. Add these
   lines to `~/.config/xdg-desktop-portal/portals.conf`, or to the
   `DESKTOP-portals.conf` file for your desktop, such as
   `~/.config/xdg-desktop-portal/KDE-portals.conf`:

   ```ini
   [preferred]
   org.freedesktop.impl.portal.FileChooser=musheen
   ```

   Then restart the portal: `systemctl --user restart xdg-desktop-portal`.

Musheen does not write this file for you. To turn the backend off, remove the
line, or set the setting back to **System**.

## What the chooser does

- **Open** picks one file, several files when the app allows it, or a folder
  when the app asks for one. Clicking a folder opens it; in a folder request,
  double-click a folder to open it.
- **Save** picks a folder and a name, starting from the app's suggested
  folder and name.
- **Save Many** picks a folder for the files the app names.
- Saving asks before it replaces a file.
- The app's file types are offered above the name, starting with the one the
  app chose.
- Only a choice you confirm goes back to the app. Cancel, Escape or closing
  the window cancels the request.

## How it runs

The package installs `musheen.portal` and a D-Bus activation file. When an
app asks for a file and Musheen is not running, the portal starts
`musheen --portal-backend`, which opens only the chooser and keeps running for
later requests. When Musheen is already running with the setting on, it
serves the requests itself.

Only the portal service may call the backend. Musheen refuses calls from any
other program on the session bus.
