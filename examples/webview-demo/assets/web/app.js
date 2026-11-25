const decode = (bytes) => new TextDecoder().decode(bytes);

document.getElementById("ask").addEventListener("click", async () => {
  const out = document.getElementById("answer");
  try {
    out.textContent = decode(await istmo.invoke("demo", "time"));
  } catch (e) {
    out.textContent = "failed: " + e.message;
  }
});

istmo.addEventListener("message", (event) => {
  const item = document.createElement("li");
  item.textContent = decode(event.data);
  document.getElementById("inbox").append(item);
});
