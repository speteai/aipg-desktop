// Runs before the page is drawn, so a phone never shows the desktop layout
// for a moment and the start logo is there from the first frame.
if (/Android/i.test(navigator.userAgent)) document.documentElement.classList.add("mobile");
