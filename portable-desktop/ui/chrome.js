const invoke = window.__TAURI__.core.invoke;
const statusElement = document.getElementById('status');
const report = (error) => { statusElement.textContent = String(error); statusElement.classList.add('failed'); };
const action = (value) => invoke('window_action', { action: value }).catch(report);
for (const value of ['minimize', 'maximize', 'close']) {
  document.getElementById(value).addEventListener('click', () => action(value));
}
document.getElementById('drag').addEventListener('mousedown', (event) => {
  if (event.button === 0 && event.detail === 1) action('drag');
});
document.getElementById('drag').addEventListener('dblclick', () => action('maximize'));
window.__TAURI__.event.listen('wrapper-status', ({ payload }) => {
  statusElement.textContent = payload;
  statusElement.title = payload;
  statusElement.classList.toggle('failed', payload.startsWith('Error:'));
}).catch(report);
invoke('shell_info').then(({ name, status }) => {
  document.getElementById('name').textContent = name;
  document.title = name;
  statusElement.textContent = status;
}).catch(report);
