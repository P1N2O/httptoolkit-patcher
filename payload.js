// Patcher payload - node builtins only (no npm dependencies needed), dynamic
// import works in both ESM and CJS. Injected into the app's build/index.js
// right before `const APP_URL =` by the Rust patcher.
const __patcherHttps = await import('node:https');
const __patcherHttp = await import('node:http');
const __patcherNet = await import('node:net');
const __patcherTls = await import('node:tls');
const __patcherFs = await import('node:fs');
const __patcherOs = await import('node:os');
const __patcherPath = await import('node:path');

// The fake pro user we inject everywhere
const __patcherProUser = {
  email,
  subscription: {
    status: 'active',
    expiry: new Date('6767-06-07').toISOString(),
    sku: 'pro-annual',
  },
  userId: 'patcher',
  banned: false,
  featureFlags: [],
  teamSubscription: null,
};

// CONNECT proxy agent (replaces https-proxy-agent)
const __patcherProxyAgent = class extends __patcherHttp.Agent {
  constructor(proxyUrl) {
    super();
    const u = new URL(proxyUrl);
    this.proxyHost = u.hostname;
    this.proxyPort = u.port ? parseInt(u.port, 10) : (u.protocol === 'https:' ? 443 : 80);
  }
  createConnection(options, callback) {
    const host = options.host || options.hostname;
    const port = options.port || 443;
    const socket = __patcherNet.connect(this.proxyPort, this.proxyHost, () => {
      socket.write(`CONNECT ${host}:${port} HTTP/1.1\r\nHost: ${host}:${port}\r\n\r\n`);
    });
    let head = Buffer.alloc(0);
    const onData = chunk => {
      head = Buffer.concat([head, chunk]);
      const idx = head.indexOf('\r\n\r\n');
      if (idx < 0) return;
      socket.removeListener('data', onData);
      const statusLine = head.slice(0, idx).toString().split('\r\n')[0];
      if (!statusLine.includes(' 200')) {
        socket.destroy();
        return callback(new Error('Proxy CONNECT failed: ' + statusLine));
      }
      const tlsSocket = __patcherTls.connect({ socket, servername: host }, () => callback(null, tlsSocket));
    };
    socket.on('data', onData);
    socket.on('error', err => callback(err));
  }
};

// Simple HTTP request helper with redirect support
const __patcherRequest = (method, url, redirectCount = 0) => new Promise((resolve, reject) => {
  const agent = globalProxy ? new __patcherProxyAgent(globalProxy.startsWith('http') ? globalProxy.replace(/^http:/, 'https:') : 'https://' + globalProxy) : undefined;

  const req = __patcherHttps.request(url, { method, agent }, res => {
    const chunks = [];
    res.on('data', chunk => chunks.push(chunk));
    res.on('end', () => {
      const data = Buffer.concat(chunks);
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        if (redirectCount >= 5) return reject(new Error('Too many redirects'));
        return resolve(__patcherRequest(method, res.headers.location, redirectCount + 1));
      }
      resolve({ data, statusCode: res.statusCode, headers: res.headers });
    });
  });
  req.on('error', reject);
  req.end();
});

// Quick check if we can reach the HTTP Toolkit servers
const __patcherHasInternet = () => __patcherRequest('HEAD', 'https://app.httptoolkit.tech')
  .then(r => r.statusCode >= 200 && r.statusCode < 400)
  .catch(() => false);

const __patcherPort = process.env.PATCHER_PORT || 5067;
const __patcherTempPath = __patcherPath.join(__patcherOs.tmpdir(), 'httptoolkit-patch');

// Point the app to our local server instead of the real one
process.env.APP_URL = `http://localhost:${__patcherPort}`;
console.log(`[Patcher] Temp path: ${__patcherTempPath}`);

// Minimal sendFile replacement: serve bytes with a content type
const __patcherMime = {
  html: 'text/html', htm: 'text/html', js: 'application/javascript', mjs: 'application/javascript',
  css: 'text/css', json: 'application/json', map: 'application/json', txt: 'text/plain',
  png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', svg: 'image/svg+xml',
  webp: 'image/webp', ico: 'image/x-icon', woff: 'font/woff', woff2: 'font/woff2', ttf: 'font/ttf',
  wasm: 'application/wasm', xml: 'application/xml', pdf: 'application/pdf', mp4: 'video/mp4', mp3: 'audio/mpeg',
};
const __patcherSendFile = (res, filePath, headers) => {
  const data = __patcherFs.readFileSync(filePath);
  if (headers) {
    for (const [key, value] of Object.entries(headers)) {
      // hop-by-hop and body-meta headers must not be copied: node computes
      // Content-Length/Transfer-Encoding itself and would reject the response
      if (['transfer-encoding', 'connection', 'content-length', 'keep-alive', 'upgrade', 'trailer', 'te', 'proxy-authenticate', 'proxy-authorization'].includes(key.toLowerCase())) continue;
      res.setHeader(key, value);
    }
  }
  const ext = __patcherPath.extname(filePath).slice(1).toLowerCase();
  res.setHeader('Content-Type', __patcherMime[ext] || 'application/octet-stream');
  res.setHeader('Content-Length', data.length);
  res.end(data);
};

const __patcherApp = __patcherHttp.createServer(async (req, res) => {
  let pathname = '';
  try {
    pathname = new URL(req.url, process.env.APP_URL).pathname;
  } catch (e) {
    pathname = req.url;
  }

  // LAYER 2: Intercept account API calls directly
  // Handles any endpoint the account store calls, so even if JS regex fails, app still gets pro user
  if (pathname.startsWith('/__patcher_api/')) {
    console.log(`[Patcher] API intercept: ${req.url}`);
    const user = JSON.parse(JSON.stringify(__patcherProUser));
    user.subscription.expiry = new Date(user.subscription.expiry);
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Access-Control-Allow-Headers', '*');
    res.setHeader('Content-Type', 'application/json');
    return res.end(JSON.stringify(user));
  }

  console.log(`[Patcher] ${req.url}`);

  let filePath = __patcherPath.join(__patcherTempPath, pathname === '/' ? 'index.html' : pathname);

  // Some routes need .html extension
  if (['/view', '/intercept', '/settings', '/mock'].includes(pathname)) {
    filePath += '.html';
  }

  // Block service worker - it causes caching headaches but needs to return 200 dummy script to prevent startup error
  if (pathname === '/ui-update-worker.js') {
    res.setHeader('Content-Type', 'application/javascript');
    res.statusCode = 200;
    return res.end('self.addEventListener("install", () => self.skipWaiting()); self.addEventListener("activate", () => self.clients.claim());');
  }

  if (!__patcherFs.existsSync(__patcherTempPath)) {
    __patcherFs.mkdirSync(__patcherTempPath);
  }

  // Offline mode - serve from cache if no internet
  if (!(await __patcherHasInternet())) {
    if (__patcherFs.existsSync(filePath)) {
      return __patcherSendFile(res, filePath);
    }
    res.statusCode = 404;
    return res.end('No internet and file not cached');
  }

  try {
    // Check if we have a cached copy that's still fresh
    if (__patcherFs.existsSync(filePath)) {
      try {
        const remoteDate = await __patcherRequest('HEAD', `https://app.httptoolkit.tech${req.url}`).then(r => new Date(r.headers['last-modified']));
        if (remoteDate < new Date(__patcherFs.statSync(filePath).mtime)) {
          return __patcherSendFile(res, filePath);
        }
      } catch (e) { /* ignore, just redownload */ }
    }

    // Download from the real server
    const remoteFile = await __patcherRequest('GET', `https://app.httptoolkit.tech${req.url}`);

    // Create directories if needed
    __patcherFs.mkdirSync(__patcherPath.dirname(filePath), { recursive: true });

    let data = remoteFile.data;

    // LAYER 1: Patch main.js JS directly
    if (pathname === '/main.js') {
      console.log(`[Patcher] Patching main.js...`);
      res.setHeader('Cache-Control', 'no-store');

      data = data.toString();

      // LAYER 1a: Brute-force replace all paid-user checks with stubs on Object.prototype
      // This guarantees isPaidUser() etc. always return true even on plain JSON objects
      const stubMethods = [
        'Object.defineProperty(Object.prototype,"__p_isPaid",{value:function(){return true},configurable:true,writable:true,enumerable:false});',
        'Object.defineProperty(Object.prototype,"__p_hasSub",{value:function(){return true},configurable:true,writable:true,enumerable:false});',
        'Object.defineProperty(Object.prototype,"__p_isPastDue",{value:function(){return false},configurable:true,writable:true,enumerable:false});',
      ].join('');

      let beforeCount = data.length;
      data = data.replaceAll('.isPaidUser()', '.__p_isPaid()');
      data = data.replaceAll('.userHasSubscription()', '.__p_hasSub()');
      data = data.replaceAll('.isPastDueUser()', '.__p_isPastDue()');

      if (data.length !== beforeCount) {
        data = stubMethods + data;
        console.log(`[Patcher] Brute-force method stubs injected`);
      }

      // LAYER 1c: Keep the MCP / remote-control bridge working. Because we fake a
      // logged-in Pro user without a real account JWT, the UI's `userJwt` getter
      // throws "No JWT found for logged in user", which crashes the operations
      // client before it can register with the server bridge. Hand it a dummy
      // token instead — the server side (patched in index.js) accepts any JWT.
      const jwtThrow = 'throw new Error("No JWT found for logged in user")';
      if (data.includes(jwtThrow)) {
        data = data.replace(jwtThrow, 'return"htk-patched-jwt"');
        console.log(`[Patcher] Neutralised userJwt throw for MCP bridge`);
      }

      // LAYER 1b: Also try regex-based user data override for deeper integration
      const accStoreName =
        data.match(/class ([0-9A-Za-z_$]+)\s*\{\s*constructor\s*\(\s*e\s*\)\s*\{\s*this\.goToSettings\s*=\s*e/)?.[1] ||
        data.match(/class ([0-9A-Za-z_$]+)[^{]*\{[^}]{0,200}goToSettings/)?.[1];

      const modName =
        data.match(/([0-9A-Za-z_$]+)\.(getLatestUserData|getLastUserData)/)?.[1];

      const userJson = JSON.stringify(__patcherProUser);
      const userInit = `const __pUser=${userJson};__pUser.subscription.expiry=new Date(__pUser.subscription.expiry);__pUser.__p_isPaid=()=>true;__pUser.__p_hasSub=()=>true;__pUser.__p_isPastDue=()=>false;`;

      if (!accStoreName) {
        console.error(`[Patcher] Couldn't find account store class - relying on API interception + brute-force stubs`);
      } else if (!modName) {
        console.error(`[Patcher] Couldn't find user data module - relying on API interception + brute-force stubs`);
      } else {
        const classPattern = new RegExp(`class ${accStoreName}\\s*\\{`);
        let patched = data.replace(
          classPattern,
          `["getLatestUserData","getLastUserData"].forEach(p=>Object.defineProperty(${modName},p,{value:()=>__pUser}));class ${accStoreName}{`
        );

        if (patched === data) {
          console.error(`[Patcher] JS inject failed - relying on API interception + brute-force stubs`);
        } else {
          data = userInit + patched;
          console.log(`[Patcher] main.js patched successfully (regex + brute-force)!`);
        }
      }
    }

    __patcherFs.writeFileSync(filePath, data);
    __patcherSendFile(res, filePath, remoteFile.headers);
  } catch (e) {
    console.error(`[Patcher] Error: ${e.message}`);
    res.statusCode = 500;
    res.end('Something went wrong');
  }
});

__patcherApp.listen(__patcherPort, () => console.log(`[Patcher] Running on port ${__patcherPort}`));

// Fix CORS and intercept account API at the Electron session level
app.on('ready', () => {
  // Remember each request's real browser Origin so we can reflect it back in the
  // CORS response. Reflecting (instead of hard-coding an origin) is what lets the
  // account/pricing fetches survive being redirected to our local __patcher_api:
  // a cross-origin redirect taints the origin, so the response must echo whatever
  // the browser actually sent, with credentials allowed.
  const __patcherOriginByReq = new Map();

  session.defaultSession.webRequest.onBeforeSendHeaders((details, callback) => {
    // Block telemetry
    const blocked = ['events.httptoolkit.tech'];
    try {
      const host = new URL(details.url).hostname;
      if (blocked.includes(host) || details.url?.includes('sentry')) {
        return callback({ cancel: true });
      }
    } catch (e) { }

    const origin = details.requestHeaders.Origin ?? details.requestHeaders.origin
    if (origin) __patcherOriginByReq.set(details.id, origin)

    // The local HTTP Toolkit server (127.0.0.1) gates every endpoint — the REST/
    // GraphQL API AND the MCP/remote-control /ui-operations WebSocket — on the
    // request Origin being exactly https://app.httptoolkit.tech (prod builds).
    // Our UI runs on localhost, so spoof the Origin for requests to that server
    // only. We must NOT spoof it for requests to our own proxy (localhost:${port}),
    // because that breaks the browser CORS check on the redirected account API.
    let host = ''
    try { host = new URL(details.url).hostname } catch (e) { }
    if (host === '127.0.0.1') {
      details.requestHeaders.Origin = 'https://app.httptoolkit.tech'
    }

    callback({ requestHeaders: details.requestHeaders });
  });

  // LAYER 2: Redirect any account/auth API calls to our fake endpoint
  session.defaultSession.webRequest.onBeforeRequest(
    { urls: ['https://api.httptoolkit.tech/*', 'https://accounts.httptoolkit.tech/*'] },
    (details, callback) => {
      try {
        const url = new URL(details.url);
        const redirectURL = `http://localhost:${__patcherPort}/__patcher_api${url.pathname}${url.search}`;
        console.log(`[Patcher] Redirecting API: ${details.url} -> ${redirectURL}`);
        callback({ redirectURL });
      } catch (e) {
        callback({});
      }
    }
  );

  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    const headers = details.responseHeaders ?? {};
    // Drop any existing CORS headers (from the real server or our own express app)
    // so we don't end up with duplicate/conflicting values.
    for (const key of Object.keys(headers)) {
      if (/^access-control-allow-(origin|credentials)$/i.test(key)) delete headers[key];
    }
    const origin = __patcherOriginByReq.get(details.id) ?? `http://localhost:${__patcherPort}`;
    __patcherOriginByReq.delete(details.id);
    headers['Access-Control-Allow-Origin'] = [origin];
    headers['Access-Control-Allow-Credentials'] = ['true'];
    callback({ responseHeaders: headers });
  });
});
