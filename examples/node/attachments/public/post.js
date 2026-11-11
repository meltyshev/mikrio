"use strict";

const result = document.querySelector("#result");
let issuedForm;
let currentDemoId;

async function requestApi(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-attachment-request": "1",
    },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    throw new Error("Sign in and check that POST demo cleanup is enabled.");
  }

  return response.json();
}

document.querySelector("#issue").addEventListener("submit", async (event) => {
  event.preventDefault();

  try {
    issuedForm = await requestApi("/api/post-demo/forms", {
      status: document.querySelector("#status").value,
    });

    currentDemoId = issuedForm.id;

    for (const id of ["send", "read", "delete"]) {
      document.querySelector("#" + id).disabled = false;
    }

    result.textContent =
      "Form ready. Expires at " +
      new Date(issuedForm.expiresAt).toLocaleTimeString() +
      ". Cleanup begins after " +
      new Date(issuedForm.cleanupAt).toLocaleTimeString() +
      ".";
  } catch (error) {
    result.textContent = error.message;
  }
});

document.querySelector("#upload").addEventListener("submit", async (event) => {
  event.preventDefault();
  const file = document.querySelector("#file").files[0];

  if (!issuedForm || !file || file.size > issuedForm.maxFileBytes) {
    result.textContent = "Issue a form and choose a file up to 1 MiB.";
    return;
  }

  const formData = new FormData();

  for (const [name, value] of Object.entries(issuedForm.form.fields)) {
    formData.append(name, value);
  }

  formData.append("file", file); // File last; preserve every SDK-generated field.

  try {
    const response = await fetch(issuedForm.form.url, {
      method: "POST",
      body: formData,
      redirect: "manual",
      credentials: "omit",
    });

    await response.arrayBuffer();

    result.textContent = response.ok
      ? "Publication acknowledged. Read back the current object, or choose another file to replay."
      : "No readable success acknowledgment (status " +
        response.status +
        "). Check readback; the object may have been published.";
  } catch {
    result.textContent =
      "The response was unavailable. Check readback before assuming the upload failed.";
  }
});

document.querySelector("#read").addEventListener("click", async () => {
  if (!currentDemoId) {
    return;
  }

  try {
    const { url } = await requestApi(
      "/api/post-demo/" + currentDemoId + "/download",
      {},
    );

    const response = await fetch(url, { credentials: "omit" });

    if (!response.ok) {
      await response.arrayBuffer();

      result.textContent =
        "Readback found no accessible object (status " + response.status + ").";

      return;
    }

    const bytes = await response.arrayBuffer();

    result.textContent =
      "Current object: " +
      bytes.byteLength +
      " bytes. Content: " +
      new TextDecoder().decode(bytes).slice(0, 2048);
  } catch {
    result.textContent =
      "Readback is unavailable; retry after checking the connection.";
  }
});

document.querySelector("#delete").addEventListener("click", async () => {
  if (!currentDemoId) {
    return;
  }

  try {
    await requestApi("/api/post-demo/" + currentDemoId + "/delete", {});

    result.textContent =
      "Delete acknowledged. The form remains reusable until expiry; late writes are handled by recurring cleanup.";
  } catch {
    result.textContent = "Delete outcome is unknown. Check readback and retry.";
  }
});
