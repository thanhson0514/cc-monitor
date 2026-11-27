// Docs pages: EN/VI switch (same "ccmon-lang" key as the landing page) and the
// "on this page" highlight. Loaded in <head> so the language is set before first paint.
(function () {
  var lang = null;
  try { lang = localStorage.getItem('ccmon-lang'); } catch (e) {}
  if (lang !== 'en' && lang !== 'vi') lang = (navigator.language || '').indexOf('vi') === 0 ? 'vi' : 'en';
  document.documentElement.lang = lang;

  function apply(l) {
    document.documentElement.lang = l;
    var t = document.querySelector('title');
    if (t) {
      if (!t.dataset.en) t.dataset.en = t.textContent;
      document.title = l === 'vi' && t.dataset.vi ? t.dataset.vi : t.dataset.en;
    }
    document.querySelectorAll('.lang button').forEach(function (b) { b.classList.toggle('on', b.dataset.lang === l); });
    try { localStorage.setItem('ccmon-lang', l); } catch (e) {}
  }

  document.addEventListener('DOMContentLoaded', function () {
    apply(document.documentElement.lang);
    document.querySelectorAll('.lang button').forEach(function (b) {
      b.addEventListener('click', function () { apply(b.dataset.lang); });
    });

    var links = {};
    document.querySelectorAll('.doc-side .toc a[href^="#"]').forEach(function (a) { links[a.getAttribute('href').slice(1)] = a; });
    var secs = document.querySelectorAll('.prose section[id]');
    if (!('IntersectionObserver' in window) || !secs.length) return;
    var visible = {};
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) { visible[e.target.id] = e.isIntersecting; });
      var first = null;
      secs.forEach(function (s) { if (!first && visible[s.id]) first = s.id; });
      if (!first) return;
      Object.keys(links).forEach(function (id) { links[id].classList.toggle('on', id === first); });
    }, { rootMargin: '-70px 0px -55% 0px' });
    secs.forEach(function (s) { io.observe(s); });
  });
})();
