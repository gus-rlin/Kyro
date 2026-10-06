import service from '../electron/chat.cjs';

export function chatDevelopment(chat = service.createChatService(), localOrigin = 'http://127.0.0.1:5174') {
  const expected = new URL(localOrigin);
  if (expected.hostname !== '127.0.0.1') throw new Error('Local chat only');
  return {
    name: 'kyro-chat-development', apply: 'serve',
    configureServer(server) {
      server.httpServer?.once('close',()=>{ void chat.close?.().catch(()=>{}); });
      server.middlewares.use('/__kyro_chat', async (req, res) => {
        res.setHeader('Cache-Control', 'no-store');
        res.setHeader('Content-Type', 'application/json; charset=utf-8');
        if (req.method !== 'POST' || req.headers.host !== expected.host || req.headers.origin !== expected.origin || req.headers['x-kyro-local'] !== '1' || req.headers['content-type'] !== 'application/json' || !['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(req.socket.remoteAddress)) {
          res.statusCode = 403; res.end(JSON.stringify({ error: 'Accès local refusé.' })); return;
        }
        const controller = new AbortController();
        res.once('close', () => controller.abort());
        try {
          const action = req.url?.slice(1);
          if (!['status', 'send', 'cancel', 'watch', 'plansProjects', 'plansStatus', 'plansList', 'plansStart', 'plansRead', 'plansExecute', 'plansCancel', 'plansUsage'].includes(action)) { res.statusCode = 404; res.end('{}'); return; }
          let body = '', bytes = 0;
          const decoder = new TextDecoder('utf-8', { fatal: true });
          for await (const chunk of req) {
            bytes += chunk.length;
            if (bytes > 35000) { res.statusCode = 413; res.end('{}'); return; }
            body += decoder.decode(chunk, { stream: true });
          }
          body += decoder.decode();
          const value = JSON.parse(body);
          if (action === 'watch') {
            await chat.watch(value.jobId, value.after, async (event) => {
              if (!res.headersSent) { res.setHeader('Content-Type', 'text/event-stream; charset=utf-8'); res.flushHeaders(); }
              if (!res.write(`data: ${JSON.stringify(event)}\n\n`)) {
                await new Promise((resolve) => { res.once('drain', resolve); controller.signal.addEventListener('abort', resolve, {once:true}); });
              }
            }, controller.signal);
            res.end();
          } else res.end(JSON.stringify({ value: await chat[action](value) }));
        } catch (error) {
          if (controller.signal.aborted) return;
          if (res.headersSent) res.end();
          else res.end(JSON.stringify({ error: error.message, code:error.code }));
        }
      });
    },
  };
}
