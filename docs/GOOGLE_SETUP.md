# Google sign-in and Drive setup

PDF Kernel signs users in with Google and stores their PDFs and chats in
**their own** Google Drive. You need one free OAuth client from Google Cloud.
No server, no Firebase.

## 1. Create a Google Cloud project

1. Open https://console.cloud.google.com/ and create a project (e.g. "PDF Kernel").
2. **APIs & Services → Library**, search **Google Drive API**, click **Enable**.

## 2. Configure the consent screen

**APIs & Services → OAuth consent screen** (called "Google Auth Platform" in newer consoles):

1. User type: **External**.
2. App name, support email, developer email.
3. **Data access / Scopes**: add
   - `openid`, `.../auth/userinfo.email`, `.../auth/userinfo.profile`
   - `.../auth/drive.file`: "See, edit, create and delete only the specific Google Drive files you use with this app"

   All of these are **non-sensitive**, so Google requires no security
   assessment. The app never sees the rest of a user's Drive.
4. **Audience / Test users**: while the app is in *Testing*, add the Google
   accounts that may sign in (up to 100).

> In *Testing* mode Google expires sign-ins after **7 days**, so users must
> sign in again weekly. Click **Publish app** to remove that limit. With only
> non-sensitive scopes, publishing needs no security review. Google may ask
> you to verify the app name and logo if you show them on the consent screen.

## 3. Create the OAuth client

**APIs & Services → Credentials → Create credentials → OAuth client ID**

- Application type: **Desktop app**
- Name: anything

Copy the **Client ID** and **Client secret**.

One Desktop client is used on every platform, including Android. That matters
because `drive.file` access is tied to the OAuth client: the same client
means the same files on every device. Google documents that a desktop
client's "secret" is not confidential; the sign-in is protected by PKCE.

## 4. Give the credentials to the app

**Local development:** add to `.env` in the repo root (or your user config dir):

```
GOOGLE_CLIENT_ID=1234-abc.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=GOCSPX-...
```

**Release builds (GitHub Actions):** repo **Settings → Secrets and variables → Actions**:

| Secret | Value |
|---|---|
| `GOOGLE_CLIENT_ID` | client ID |
| `GOOGLE_CLIENT_SECRET` | client secret |

They are compiled into the app (`PDFK_GOOGLE_CLIENT_ID` / `PDFK_GOOGLE_CLIENT_SECRET`).
Without them the app builds fine but offers only "This device" storage.

## Android signing (optional)

Without a key, CI signs the APK with a throwaway key: it installs, but each new
build needs an uninstall first. To keep one key:

```sh
keytool -genkeypair -keystore release.jks -alias pdfkernel -keyalg RSA -keysize 2048 -validity 10000
base64 -w0 release.jks   # macOS: base64 -i release.jks
```

Add secrets `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD` and
`ANDROID_KEY_ALIAS`. Keep `release.jks` safe: losing it means users must
reinstall to update.

## How sign-in works

1. The app opens the system browser at Google's sign-in page.
2. Google redirects to `http://127.0.0.1:<random port>` where the app is listening.
3. The app exchanges the code (with PKCE) for tokens and checks Drive access was granted.
4. The refresh token goes into the OS keychain (Windows Credential Manager,
   macOS Keychain, Linux Secret Service) or, on Android, app-private storage.
   Access tokens stay in memory.

On Android, Google shows the result in the browser; switch back to the app if
it doesn't come forward on its own. A native Android sign-in (Credential
Manager) would smooth this step and is a planned improvement.

## Where files go

```
My Drive/
  PDF Kernel/
    report.pdf
    report.pdf.chat.json
```

Users can see, download or delete these in Drive. Deleting in the app moves
them to Drive's trash.
