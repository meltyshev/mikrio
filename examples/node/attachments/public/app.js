const login = document.querySelector("#login");
const account = document.querySelector("#account");
const status = document.querySelector("#status");
const retry = document.querySelector("#retry");
let isBusy = false;

async function requestApi(path, body) {
  const response = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: {
      "content-type": "application/json",
      "x-attachment-request": "1",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
    cache: "no-store",
  });

  const value = await response.json();

  if (!response.ok) {
    throw new Error(value.error);
  }

  return value;
}

async function runAction(work) {
  if (isBusy) {
    return;
  }

  isBusy = true;

  document.querySelectorAll("button").forEach((button) => {
    button.disabled = true;
  });

  try {
    await work();
  } catch (error) {
    if (error instanceof Error && error.message === "unauthorized") {
      login.hidden = false;
      account.hidden = true;
    }

    const messages = {
      unauthorized: "Please sign in with your application account.",
      invalid:
        "Check the file: it must be valid UTF-8 text, 1 byte to 1 MiB, without binary control characters.",
      notFound: "This attachment is unavailable for your account.",
      conflict:
        "This attempt cannot attach. Choose the file again to start a new attempt.",
      unavailable: "The service is unavailable. Please try again.",
    };

    status.textContent =
      messages[error.message] ??
      "The request did not finish. Check the previous upload before starting again.";
  } finally {
    isBusy = false;

    document.querySelectorAll("button").forEach((button) => {
      button.disabled = false;
    });

    retry.hidden = !sessionStorage.getItem("attachment-attempt");
  }
}

async function refresh() {
  const session = await requestApi("/api/session");
  document.querySelector("#post-demo-link").hidden = !session.postDemo;
  login.hidden = true;
  account.hidden = false;
  document.querySelector("#username").textContent = session.username;

  const { attachments } = await requestApi("/api/attachments");
  const list = document.querySelector("#attachments");
  list.replaceChildren();

  if (!attachments.length) {
    list.textContent = "No attachments yet.";
  }

  for (const attachment of attachments) {
    const item = document.createElement("li");
    const label = document.createElement("span");
    label.textContent = `${attachment.filename} - ${attachment.contentLength} bytes`;

    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "Download";

    button.addEventListener("click", () =>
      runAction(async () => {
        const { url } = await requestApi(
          `/api/attempts/${attachment.attachmentId}/download`,
          {},
        );

        const link = document.createElement("a");
        link.href = url;
        link.rel = "noreferrer noopener";
        link.click();
      }),
    );

    item.append(label, button);

    if (session.publicAttachments) {
      const publication = document.createElement("span");
      publication.textContent = `Requested ${attachment.visibility} - ${attachment.publication}`;

      const change = document.createElement("button");
      change.type = "button";

      change.textContent =
        attachment.visibility === "public" ? "Make private" : "Publish";

      change.addEventListener("click", () =>
        runAction(async () => {
          await requestApi(
            `/api/attachments/${attachment.attachmentId}/visibility`,
            {
              visibility:
                attachment.visibility === "public" ? "private" : "public",
            },
          );

          status.textContent =
            "Visibility change queued. Refresh to check its status. Previously downloaded or cached bytes cannot be recalled.";

          await refresh();
        }),
      );

      item.append(publication, change);
    }

    list.append(item);
  }
}

async function finalize(attemptId) {
  await requestApi(`/api/attempts/${attemptId}/finalize`, {});
  sessionStorage.removeItem("attachment-attempt");
  status.textContent = "File verified and attached.";
  await refresh();
}

login.addEventListener("submit", (event) => {
  event.preventDefault();

  void runAction(async () => {
    const fields = new FormData(login);

    await requestApi("/api/login", {
      username: fields.get("username"),
      password: fields.get("password"),
    });

    login.reset();
    status.textContent = "Signed in.";
    await refresh();
  });
});

document.querySelector("#logout").addEventListener("click", () =>
  runAction(async () => {
    await requestApi("/api/logout", {});
    sessionStorage.removeItem("attachment-attempt");
    account.hidden = true;
    login.hidden = false;
    status.textContent = "Signed out.";
  }),
);

document
  .querySelector("#refresh")
  .addEventListener("click", () => runAction(refresh));

document.querySelector("#upload").addEventListener("submit", (event) => {
  event.preventDefault();

  void runAction(async () => {
    const file = document.querySelector("#file").files[0];

    if (!file || file.size < 1 || file.size > 1024 * 1024) {
      throw new Error("invalid");
    }

    const digest = await crypto.subtle.digest(
      "SHA-256",
      await file.arrayBuffer(),
    );

    const sha256 = Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join("");

    const attempt = await requestApi("/api/attempts", {
      filename: file.name,
      contentLength: file.size,
      sha256,
    });

    sessionStorage.setItem("attachment-attempt", attempt.attemptId);
    status.textContent = "Uploading...";

    try {
      const response = await fetch(attempt.upload.url, {
        method: "PUT",
        headers: attempt.upload.headers,
        body: file,
        credentials: "omit",
        referrerPolicy: "no-referrer",
      });

      // A failed/lost response is reconciled by an authorized content read below.
      if (!response.ok && response.status !== 412) {
        throw new Error("upload");
      }
    } catch {
      status.textContent = "Checking whether the upload completed...";
    }

    await finalize(attempt.attemptId);
  });
});

retry.addEventListener("click", () =>
  runAction(async () => {
    const attemptId = sessionStorage.getItem("attachment-attempt");

    if (attemptId) {
      await finalize(attemptId);
    }
  }),
);

void runAction(async () => {
  try {
    await refresh();
  } catch {
    login.hidden = false;
    account.hidden = true;
  }
});
