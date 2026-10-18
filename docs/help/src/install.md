# Install Decurion

This page helps you install Decurion and open the board for the first time.

## What you need

- A Windows PC, a Mac, or a Linux computer.
- A Claude Max plan, or an Anthropic pay-per-use key. See [Connect your Claude accounts](connect-claude-accounts.md).

Decurion also uses four free helper programs:

- **Docker Desktop** makes the sealed workspaces your agents work in.
- **Git** keeps the history of every change to your code.
- **Claude Code** is the AI agent itself.
- **Tailscale** lets your phone reach your PC safely. You only need it for your phone.

## Install on Windows

1. Go to the [Decurion releases page](https://github.com/decurion-dev/decurion/releases).
2. Under the newest version, download the file whose name starts with **Decurion-Setup** and ends with **.exe**.
3. Open the file you downloaded.
4. If Windows shows "Windows protected your PC", click **More info**, then click **Run anyway**. Windows shows this because the installer does not have a digital signature yet.
5. Follow the steps in the installer. It installs Decurion for your Windows user and adds two entries to your Start menu:
   - **Decurion** starts Decurion and opens your board.
   - **Decurion - Save a Claude sign-in** saves your Claude accounts for Decurion. See [Connect your Claude accounts](connect-claude-accounts.md).
6. If any helper programs are missing, the installer lists them on a page called **Helpers Decurion needs**. Leave each one checked to install it. The installer uses **winget**, the app installer built into Windows. Some helpers may ask for your permission.
7. If Docker Desktop was just installed, restart your PC. Then open Docker Desktop once and finish its own setup.
8. In Docker Desktop, open **Settings**, then **General**. Make sure **Expose daemon on tcp://localhost:2375 without TLS** is **off**. Agents cannot reach your PC, but other programs in Docker can, and with this on they could take control of Docker.

## Start Decurion for the first time

1. Open the Start menu and click **Decurion**. A window opens. If Docker Desktop is not running yet, Decurion starts it and waits up to 3 minutes for it.
2. The first time, Decurion gets the agent workspace and the agents' website gate ready. This takes a few minutes. It happens again only when you install a new version.
3. Your browser opens a page called **Sign in to Decurion**. Click **Sign in**. You only do this the first time.

Keep the Decurion window open while you use it. Close the window to stop Decurion.

After that, clicking **Decurion** in the Start menu opens your board directly. Your browser stays signed in, even after Decurion restarts. It stays signed in as long as you use it at least once a year.

If the board does not open, the window shows a link. Paste it into Chrome or Edge. This link works only until Decurion restarts. Use it only on your own PC.

## Install on Mac or Linux

1. Install Docker, Git and Claude Code. The Mac and Linux download does not install them for you.
2. Go to the [Decurion releases page](https://github.com/decurion-dev/decurion/releases).
3. Download the **.tar.gz** file for your computer.
4. Unpack it. On a Mac, double-click it. On Linux, open a terminal and run `tar -xzf` followed by the file name.
5. Open a terminal in the unpacked folder and run `./install.sh`. This puts Decurion in the `.local` folder in your home folder.
   - To install it for everyone on the computer instead, run `sudo ./install.sh --prefix /usr/local`. This asks for your password.
6. Save your Claude tokens. On Mac and Linux this works differently from Windows. See [Connect your Claude accounts](connect-claude-accounts.md).
7. Run `decurion-start`. If the terminal says the command is not found, use the full path that `install.sh` showed at the end, such as `~/.local/bin/decurion-start`.
8. The first time, Decurion gets the agent workspace and the agents' website gate ready. This takes a few minutes.
9. The first time, your browser opens a page called **Sign in to Decurion**. Click **Sign in**. After that, `decurion-start` opens your board directly. If nothing opens, paste the link from the terminal into Chrome or Edge.

Keep the terminal open while you use Decurion. Press Ctrl+C in it to stop Decurion.

To remove Decurion later, run `./install.sh --uninstall` from the unpacked folder. If you installed it for everyone, run `sudo ./install.sh --prefix /usr/local --uninstall`. Your saved boards and settings stay until you delete them.

## If something goes wrong

- **"Docker Desktop did not start."** Open Docker Desktop yourself, wait until it says it is running, then open Decurion again.
- **"Could not prepare the agent workspace."** or **"Could not prepare the agents' website gate."** Check that Docker Desktop is running and that you are online. Then open Decurion again.
- **"Decurion could not start."** The details are in `hub-errors.log`. On Windows it is in `%LOCALAPPDATA%\decurion\logs`. On a Mac or Linux it is in `~/decurion/logs`. See [When something fails](when-something-fails.md).
- **The board says "Not signed in."** This browser was removed in **Settings**, its cookies were cleared, or it was not used for a year. Close Decurion and open it again. Then click **Sign in** on the page that opens.
- **"This is not a Decurion sign-in link."** Part of the link is missing. Copy the whole link again.
- **"wrong or expired login code."** Decurion was restarted after the link was made. Use the newest link.
- **The link does not open.** Use Chrome or Edge. They know that `decurion.localhost` means your own PC.

Next: [Connect your Claude accounts](connect-claude-accounts.md).
