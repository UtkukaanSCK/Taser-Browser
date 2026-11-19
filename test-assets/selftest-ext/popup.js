// The popup asks for the active tab (the CSP forbids inline script).
chrome.tabs.query({ active: true, currentWindow: true }, (t) => {
  document.body.dataset.url = t[0] && t[0].url;
  document.body.dataset.id = t[0] ? typeof t[0].id : "none";
  document.body.dataset.n = String(t.length);
});
