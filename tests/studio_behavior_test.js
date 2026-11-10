'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const html = fs.readFileSync('src/studio/studio.html','utf8');
const source = html.slice(html.indexOf('function modal('),html.indexOf('async function validate('));
function setup() {
  const nodes = new Map();
  const context = vm.createContext({
    console, clearTimeout, state:{workflow:{},dirty:true,path:'old.yaml',saved:true,paths:['old.yaml'],revision:0},
    inputOverrides:new Map(),$:id => {
      if (!nodes.has(id)) nodes.set(id,{disabled:false,hidden:true});
      return nodes.get(id);
    },
  });
  vm.runInContext(source,context);
  vm.runInContext(`
    let confirmed = true;
    let calls = [];
    const empty = () => ({steps:[]});
    const fileUrl = path => '/api/workflow?path='+path;
    confirmAction = async () => confirmed;
    refreshFiles = () => calls.push('refresh');
    loadFile = async path => { calls.push(['load',path,state.dirty]); return true; };
    choosePath = async () => 'new.yaml';
    replaceFile = (path,data,saved) => { calls.push(['replace',path,state.dirty]); state.path=path;state.saved=saved;state.dirty=!saved; };
    notice = message => calls.push(message);
    api = async (path,method) => calls.push([path,method,state.dirty]);
  `,context);
  return {context,run:code => vm.runInContext(code,context)};
}
test('switch clears dirty before loading and cancel preserves the draft',async () => {
  const {run} = setup();
  await run("switchFile('other.yaml')");
  assert.equal(run("calls[0][2]"),false);
  run("state.dirty=true;confirmed=false;calls=[]");
  await run("switchFile('other.yaml')");
  assert.equal(run("state.dirty"),true);
  assert.equal(run("calls.length"),1);
});
test('new file replaces the draft after discard, and duplicate preserves it',async () => {
  const {run} = setup();
  await run('newFile()');
  assert.equal(run('calls[0][2]'),false);
  run("state.dirty=true;state.paths=['new.yaml'];calls=[]");
  await run('newFile()');
  assert.equal(run('state.dirty'),true);
});
test('delete clears dirty before the request and restores it on disconnect',async () => {
  const {run} = setup();
  await run('deleteFile()');
  assert.equal(run('calls[0][2]'),false);
  run("state.saved=true;state.dirty=true;calls=[];api=async()=>{throw new Error('offline')}");
  await run('deleteFile()');
  assert.equal(run('state.dirty'),true);
  assert.equal(run("$('run').disabled"),false);
  assert.equal(run('state.loading'),false);
});
test('switch failure restores dirty and save as tracks the server path',async () => {
  const {run} = setup();
  run('loadFile=async()=>false');
  await run("switchFile('other.yaml')");
  assert.equal(run('state.dirty'),true);
  run(`
    validCommandDraft=()=>true;payload=()=>({workflow:{}});
    api=async()=>({sha256:'hash',doctor:{ok:true}});
    applyPreview=()=>{};diskNotice=()=>{};
  `);
  await run('save(true)');
  assert.equal(run('state.path'),'new.yaml');
  assert.equal(run('state.dirty'),false);
});
test('fetch failure explains how to reconnect',async () => {
  const context = vm.createContext({fetch:async()=>{throw new Error('network')}});
  const api = html.slice(html.indexOf('async function api('),html.indexOf('const fileUrl'));
  vm.runInContext(api,context);
  await assert.rejects(vm.runInContext("api('/api/workflows')",context),/Studio server unreachable/);
});
