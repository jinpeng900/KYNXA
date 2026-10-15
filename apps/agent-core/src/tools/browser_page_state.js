/* Detect a verification page without solving its challenge or changing the user's navigation.
 * 只识别人机验证页，不求解验证，也不改变用户当前页面。
 */
(() => {
  const address = new URL(location.href);
  const google = /(^|\.)google\.[a-z.]{2,12}$/i.test(address.hostname);
  const cloudflareForm = !!document.querySelector("#challenge-form,#cf-challenge-running");
  // Ordinary sites need no full-page text/layout pass during each navigation-state poll.
  // 普通站点的状态轮询无需逐秒读取整页文字和触发布局，仅对候选验证页补查正文。
  const summary = google || cloudflareForm ? (document.body?.innerText || "").slice(0, 3000) : "";
  const googleChallenge = google && (/^\/sorry(?:\/|$)/.test(address.pathname)
    || /our systems have detected unusual traffic|我们的系统检测到.{0,45}异常流量/i.test(summary));
  const cloudflareChallenge = cloudflareForm
    && /just a moment|verify.{0,15}human|人机验证|验证您是人类/i.test(document.title + " " + summary);
  let query = address.searchParams.get("q");
  if (!query && googleChallenge) {
    const original = address.searchParams.get("continue");
    if (original) {
      try {
        const source = new URL(original, address);
        if (/(^|\.)google\.[a-z.]{2,12}$/i.test(source.hostname)) query = source.searchParams.get("q");
      } catch { /* A malformed continuation never becomes a navigation destination. 中文：无效续接地址不能成为导航目标。 */ }
    }
  }
  const needsHumanVerification = googleChallenge || cloudflareChallenge;
  return {
    viewport: { width: innerWidth, height: innerHeight, deviceScaleFactor: devicePixelRatio },
    needsHumanVerification,
    verificationProvider: googleChallenge ? "google" : cloudflareChallenge ? "cloudflare" : null,
    verificationUrl: needsHumanVerification ? address.href : null,
    alternativeSearchUrl: googleChallenge && query ? "https://www.bing.com/search?q=" + encodeURIComponent(query) : null,
    evidenceUsable: !needsHumanVerification
  };
})()
