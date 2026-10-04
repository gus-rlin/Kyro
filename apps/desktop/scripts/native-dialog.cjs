const { app, dialog } = require('electron');

// No BrowserWindow or renderer: this helper only displays the OS file dialogue.
app.whenReady().then(async () => {
  const options = {
    title: process.env.KYRO_DIALOG_TITLE || 'Choisir un dossier',
    defaultPath: process.env.KYRO_DIALOG_PATH || undefined,
    buttonLabel: process.env.KYRO_DIALOG_BUTTON || undefined,
  };
  const kind = process.argv[2];
  let result;
  if (kind === 'folder') result = await dialog.showOpenDialog({ ...options, properties: ['openDirectory'] });
  else if (kind === 'save') result = await dialog.showSaveDialog(options);
  else throw new Error('Unsupported dialogue kind');

  const path = result.canceled ? '' : kind === 'folder' ? result.filePaths[0] || '' : result.filePath || '';
  process.stdout.write(JSON.stringify({ path }), () => app.quit());
}).catch(() => {
  process.stderr.write('Impossible d’ouvrir le sélecteur système.\n', () => app.exit(1));
});
