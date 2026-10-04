import {test,expect} from '@playwright/test';
import {conversationContext,type Turn} from '../src/chat-context';
test('context drops complete oldest pairs and excludes interrupted output',()=>{
  const turns:Turn[]=[{id:'old',user:'x'.repeat(6000),assistant:'y'.repeat(6000),state:'complete'},{id:'recent',user:'Cèdre',assistant:'Nom retenu : Cèdre 🍋',state:'complete'},{id:'stopped',user:'Un échange interrompu',assistant:'Ne pas réutiliser',state:'interrupted'}];
  const context=conversationContext(turns,'Quel nom ?',8192);
  expect(context).toEqual([{role:'user',content:'Cèdre'},{role:'assistant',content:'Nom retenu : Cèdre 🍋'},{role:'user',content:'Quel nom ?'}]);
  expect(conversationContext(turns,'Bonjour',4096).at(-1)?.content).toBe('Bonjour');
  expect(()=>conversationContext([], '🍋'.repeat(5000),16384)).toThrow('trop volumineux');
});
