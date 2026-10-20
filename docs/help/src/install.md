# Install Decurion

This page helps you install Decurion and open it for the first time.

## What you need

- A Windows 10 or 11 PC, a Mac, or a Linux computer.
- A Claude Max plan, or an Anthropic pay-per-use key. See [Connect your Claude accounts](connect-claude-accounts.md).

Decurion also uses four free helper programs:

- **Docker Desktop** makes the sealed workspaces your agents work in.
- **Git** keeps the history of every change to your code.
- **Claude Code** lets Decurion sign in to your Claude accounts.
- **Tailscale** lets your phone reach your PC safely. You only need it for your phone.

## Install on Windows

1. Go to the [Decurion releases page](https://github.com/decurion-dev/decurion/releases).
2. Under the newest version, download the file whose name starts with **Decurion-Setup** and ends with **.exe**.
3. Open the file you downloaded.
4. If Windows shows "Windows protected your PC", click **More info**, then click **Run anyway**. Windows shows this because the installer does not have a digital signature yet.
5. Follow the steps in the installer. It installs Decurion for your Windows user only, so it does not ask for an administrator password.
6. On the page with extra choices, decide how Decurion starts:
   - **Start Decurion when I sign in to Windows, so your work keeps going after a restart** is checked. Leave it checked, and Decurion starts by itself in the background each time you sign in. It does not open your browser. Uncheck it if you would rather start Decurion yourself.
   - **Put a Decurion shortcut on my desktop** is not checked. Check it if you want one.
7. If any helper programs are missing, the installer shows a page called **Helpers Decurion needs**. Leave each one checked, and the installer gets it for you with **winget**, the app installer built into Windows. Some helpers may ask for your permission. If your PC has no winget, the page lists the websites to get each helper from instead.
8. On the last page, leave **Open Decurion now** checked and click **Finish**.
9. If Docker Desktop was just installed, restart your PC. Then open Docker Desktop once and finish its own setup.
10. In Docker Desktop, open **Settings**, then **General**. Make sure **Expose daemon on tcp://localhost:2375 without TLS** is **off**. Agents cannot reach your PC, but other programs in Docker can, and with this on they could take control of Docker.

The installer adds two entries to your Start menu, each with the Decurion logo:

- **Decurion** starts Decurion and opens your board.
- **Decurion - Save a Claude sign-in** saves a Claude sign-in or a pay-per-use key. See [Connect your Claude accounts](connect-claude-accounts.md).

## Start Decurion for the first time

1. Open the Start menu and click **Decurion**. A window opens. If Docker Desktop is not running yet, Decurion starts it and waits up to 3 minutes for it.
2. The first time, Decurion gets three things ready. This takes a few minutes, and the window shows each one:
   - **The agent workspace**, where Claude agents work.
   - **The workspace for ChatGPT and Grok work.** If this one fails, Claude work still runs. Decurion tries again the next time you open it.
   - **The agents' website gate**, the one door agents use to reach the internet.

   This happens again only when you install a new version.
3. Your browser opens a page called **Sign in to Decurion**. Click **Sign in**. You only do this the first time.

Keep the Decurion window open while you use it. Close the window to stop Decurion. If the part that starts agents stops, the window starts it again by itself.

After that, clicking **Decurion** in the Start menu opens your board directly. If Decurion is already running, for example because it started when you signed in to Windows, the same entry just opens your board. Your browser stays signed in, even after Decurion restarts, as long as you use it at least once a year.

When Decurion starts by itself at sign-in, its window stays small on the taskbar and waits up to 10 minutes for Docker Desktop. Close that window to stop Decurion.

If Decurion does not open in your browser, the window shows a link. Paste it into Chrome or Edge. This link works only until Decurion restarts. Use it only on your own PC.

## Install on Mac or Linux

1. Install Docker, Git and Claude Code. The Mac and Linux download does not install them for you. Install Tailscale too if you want to use your phone.
2. Go to the [Decurion releases page](https://github.com/decurion-dev/decurion/releases).
3. Download the **.tar.gz** file for your computer: the one ending in **macos-universal** for any Mac, **linux-x64** for most Linux computers, or **linux-arm64** for Linux on an ARM chip.
4. Unpack it. On a Mac, double-click it. On Linux, open a terminal and run `tar -xzf` followed by the file name.
5. Open a terminal in the unpacked folder and run `./install.sh`. This puts Decurion in the `.local` folder in your home folder.
   - To install it for everyone on the computer instead, run `sudo ./install.sh --prefix /usr/local`. This asks for your password.
6. Run `decurion-start`. If the terminal says the command is not found, use the full path that `install.sh` showed at the end, such as `~/.local/bin/decurion-start`.
7. If Docker is not running, Decurion waits for it. On a Mac, it starts Docker Desktop for you.
8. The first time, Decurion gets the agent workspace, the workspace for ChatGPT and Grok work, and the agents' website gate ready. This takes a few minutes.
9. The first time, your browser opens a page called **Sign in to Decurion**. Click **Sign in**. After that, `decurion-start` opens Decurion's **Home** directly. If nothing opens, paste the link from the terminal into Chrome or Edge.
10. Sign in to your Claude accounts in Decurion's **Settings**. See [Connect your Claude accounts](connect-claude-accounts.md).

Keep the terminal open while you use Decurion. Press Ctrl+C in it to stop Decurion. On a Mac or Linux, Decurion does not start by itself when you sign in.

## Remove Decurion

- **Windows:** open Windows **Settings**, then **Apps**. Find **Decurion** in the list of installed apps and click **Uninstall**.
- **Mac or Linux:** run `./install.sh --uninstall` from the unpacked folder. If you installed it for everyone, run `sudo ./install.sh --prefix /usr/local --uninstall`.

Your saved boards and settings stay until you delete them. See [Back up and restore](back-up-and-restore.md) for where they are.

## If something goes wrong

- **"Docker Desktop is not installed."** Install Docker Desktop from the link Decurion shows, then open Decurion again.
- **"Docker Desktop did not start."** Open Docker Desktop yourself, wait until it says it is running, then open Decurion again.
- **"Could not prepare the agent workspace."** or **"Could not prepare the agents' website gate."** Check that Docker Desktop is running and that you are online. Then open Decurion again.
- **"Could not prepare the workspace for ChatGPT and Grok work."** Claude work still runs. Check that you are online. Decurion tries again the next time you open it.
- **"Decurion could not start."** or **"Decurion is taking too long to start."** The details are in `hub-errors.log`. On Windows it is in `%LOCALAPPDATA%\decurion\logs`. On a Mac or Linux it is in `~/decurion/logs`. See [When something fails](when-something-fails.md).
- **Decurion says "Not signed in."** This browser was removed in **Settings**, its cookies were cleared, or it was not used for a year. Close Decurion and open it again. Then click **Sign in** on the page that opens.
- **"This is not a Decurion sign-in link."** Part of the link is missing. Copy the whole link again.
- **"wrong or expired login code"** Decurion was restarted after the link was made. Use the newest link.
- **The link does not open.** Use Chrome or Edge. They know that `decurion.localhost` means your own PC.

Next: [Connect your Claude accounts](connect-claude-accounts.md).
