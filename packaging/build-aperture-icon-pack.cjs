// Assemble requested export pack without building or launching Ember.
// Run with bundled Node & NODE_PATH pointing to its installed sharp package.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const sharp = require('sharp');
sharp.concurrency(1);
sharp.cache(false);
const root = path.resolve(__dirname, '../assets/brand/aperture-flame-v1');
const sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function ico(images) {
  const header = Buffer.alloc(6 + 16 * images.length);
  header.writeUInt16LE(1, 2); header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  images.forEach(({size, bytes}, i) => {
    const at = 6 + i * 16;
    header[at] = header[at + 1] = size === 256 ? 0 : size;
    header.writeUInt16LE(1, at + 4); header.writeUInt16LE(32, at + 6);
    header.writeUInt32LE(bytes.length, at + 8); header.writeUInt32LE(offset, at + 12);
    offset += bytes.length;
  });
  return Buffer.concat([header, ...images.map(row => row.bytes)]);
}
function icns(images) {
  const tags = {16:'icp4',32:'icp5',64:'icp6',128:'ic07',256:'ic08',512:'ic09',1024:'ic10'};
  const chunks = images.map(({size, bytes}) => {
    const header = Buffer.alloc(8); header.write(tags[size]); header.writeUInt32BE(bytes.length+8, 4);
    return Buffer.concat([header, bytes]);
  });
  const header = Buffer.alloc(8); header.write('icns');
  header.writeUInt32BE(8 + chunks.reduce((n, b) => n+b.length, 0), 4);
  return Buffer.concat([header,...chunks]);
}
async function main() {
  const input = fs.readFileSync(path.join(root, 'source/master-rgba.png'));
  const info = await sharp(input).metadata();
  if (!info.hasAlpha || info.width < 1024 || info.height < 1024) throw Error('RGBA master missing or too small');
  const {data, info: raw} = await sharp(input).ensureAlpha().raw().toBuffer({resolveWithObject:true});
  const monochrome = color => {
    const copy = Buffer.from(data);
    for (let i=0; i<copy.length; i+=4) {copy[i]=color[0];copy[i+1]=color[1];copy[i+2]=color[2];}
    return sharp(copy,{raw}).png().toBuffer();
  };
  const reverse = Buffer.from(data);
  for(let i=0;i<reverse.length;i+=4) {
    if(!(reverse[i]>80 && reverse[i]>reverse[i+1]*1.4)) reverse[i]=reverse[i+1]=reverse[i+2]=245;
  }
  const masters = {color:input, black:await monochrome([24,24,24]), white:await monochrome([255,255,255]),
    reverse:await sharp(reverse,{raw}).png().toBuffer()};
  const packed = [];
  for(const [variant, master] of Object.entries(masters)) {
    const dir = path.join(root, 'png',variant); fs.mkdirSync(dir,{recursive:true});
    for(const size of sizes) {
      const bytes=await sharp(master).resize(size,size).png().toBuffer();
      fs.writeFileSync(path.join(dir,`ember-${size}.png`),bytes);
      if(variant==='color') packed.push({size,bytes});
    }
  }
  fs.mkdirSync(path.join(root,'desktop'),{recursive:true});
  fs.writeFileSync(path.join(root,'desktop/ember.ico'),ico(packed.filter(row=>row.size<=256)));
  fs.writeFileSync(path.join(root,'desktop/ember.icns'),icns(packed.filter(row=>row.size!==24 && row.size!==48)));
  const apple = path.join(root,'apple/AppIcon.appiconset');fs.mkdirSync(apple,{recursive:true});
  for(const [name, master, background] of [['default',masters.color,'#f5f2ee'],['dark',masters.reverse,'#181818'],['tinted',masters.black,'#e5e5e5']]) {
    const centered=await sharp(master).resize(820,820).png().toBuffer();
    await sharp({create:{width:1024,height:1024,channels:3,background}})
      .composite([{input:centered,left:102,top:102}]).removeAlpha().png().toFile(path.join(apple,`ember-${name}.png`));
  }
  fs.writeFileSync(path.join(apple,'Contents.json'),JSON.stringify({images:[
    {filename:'ember-default.png',idiom:'universal',platform:'ios',size:'1024x1024'},
    {filename:'ember-dark.png',idiom:'universal',platform:'ios',size:'1024x1024',appearances:[{appearance:'luminosity',value:'dark'}]},
    {filename:'ember-tinted.png',idiom:'universal',platform:'ios',size:'1024x1024',appearances:[{appearance:'luminosity',value:'tinted'}]}],info:{author:'Orthic Labs',version:1}},null,2)+'\n');
  const tiles=[];
  const label = (text,x,y) => `<text x="${x}" y="${y}" font-family="sans-serif" font-size="18" fill="#555">${text}</text>`;
  let labels='';
  for(const [i,variant] of ['color','black','white','reverse'].entries()) {
    const x=24+i*230; const bg=i>=2?'#272727':'#ece9e5';
    tiles.push({input:Buffer.from(`<svg width="220" height="220"><rect width="220" height="220" rx="16" fill="${bg}"/></svg>`),left:x,top:40});
    tiles.push({input:await sharp(masters[variant]).resize(190,190).png().toBuffer(),left:x+15,top:55});
    labels+=label(variant,x,290);
  }
  for(const [i,size] of [16,24,32,48,64,128].entries()) {
    tiles.push({input:packed.find(row=>row.size===size).bytes,left:32+i*145,top:355});
    labels+=label(`${size}px`,32+i*145,515);
  }
  const base=Buffer.from(`<svg width="960" height="550"><rect width="960" height="550" fill="#faf9f7"/>${labels}</svg>`);
  await sharp(base).composite(tiles).png().toFile(path.join(root,'preview.png'));
  const checks=[];
  for(const variant of Object.keys(masters)) for(const size of sizes) {
    const filename=`png/${variant}/ember-${size}.png`;
    const bytes=fs.readFileSync(path.join(root,filename));
    const {data:rgba,info:dimensions}=await sharp(bytes).ensureAlpha().raw().toBuffer({resolveWithObject:true});
    if(dimensions.width!==size||dimensions.height!==size||rgba[3]!==0) throw Error('Size/background mismatch: '+filename);
    const center=rgba[((Math.floor(size*.53)*size)+Math.floor(size*.5))*4+3];
    if(center>16) throw Error('Central aperture obstructed: '+filename);
    checks.push({path:filename,size,sha256:digest(bytes),background_alpha:0,aperture_alpha:center});
  }
  for(const name of ['default','dark','tinted']) {
    const meta=await sharp(path.join(apple,`ember-${name}.png`)).metadata();
    if(meta.hasAlpha||meta.width!==1024||meta.height!==1024) throw Error('Apple fallback format mismatch');
  }
  fs.writeFileSync(path.join(root,'manifest.json'),JSON.stringify({schema:'ember.icon-pack.v1',master_sha256:digest(input),
    sizes,variants:Object.keys(masters),files:checks,desktop:{ico_sizes:packed.filter(row=>row.size<=256).map(row=>row.size),icns_sizes:packed.filter(row=>row.size!==24&&row.size!==48).map(row=>row.size)},apple:{size:1024,opaque:true,appearances:['default','dark','tinted']},checks:'decoded PNG dimensions, transparent corners & aperture; opaque Apple fallbacks'},null,2)+'\n');
  console.log(JSON.stringify({root,png_variants:checks.length,verified:true}));
}
main().catch(error=>{console.error(error.message);process.exitCode=1;});
