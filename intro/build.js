const fs = require('fs');

const FPS = 30, XF = 0.6;
const DUR_TITLE = 3.2, DUR_SLIDE = 4.6, DUR_END = 4.6;
const MARGIN = 0.5; // 每个输入多给 0.5s 余量，避免 xfade 报“输入过短”

const slides = [
  { img: '01-launch.png',      title: '启动',   lines: ['选版本，点「启动游戏」。', '数据目录就是 exe 旁的 .minecraft 文件夹——便携结构，整套复制就能带走。'] },
  { img: '02-download.png',    title: '下载',   lines: ['三块并行，各报各的。', '游戏核心 / 模组加载器 / 模组，一处下载、各自汇报进度。'] },
  { img: '03-multiplayer.png', title: '联机',   lines: ['6 位房号，好友直连。', 'IPv6 优先，自动 UPnP / NAT-PMP 端口映射。'] },
  { img: '04-versions.png',    title: '版本',   lines: ['本地版本集中管理。', '20+ 版本一键删除、克隆、补全；库/资源/运行时全局共享。'] },
  { img: '05-mods.png',        title: '模组',   lines: ['校验式下载。', 'Modrinth 逐文件 sha1 校验，依赖自动补齐，拒绝覆盖。'] },
  { img: '06-tools.png',       title: '工具',   lines: ['多线程下载器。', '12 路并发、断点续传，中断只丢缺失字节。'] },
  { img: '07-settings.png',    title: '设置',   lines: ['查找 Java。', '只扫安装目录与注册表，绝不全盘读写。'] },
  { img: '08-notify.png',      title: '无遥测', lines: ['默认什么都不发。', '通知默认关闭，无登录、无账号、无上报；MPL-2.0 开源。'] },
];

const L = slides.length;
const dur = i => (i === 0 ? DUR_TITLE : (i === L + 1 ? DUR_END : DUR_SLIDE));

// 片头 / 片尾静态图
const lines = [];
lines.push(`ffmpeg -y -f lavfi -i "color=c=0x0b0c0f:s=1920x1080" -frames:v 1 -filter_complex "drawtext=fontfile=font.ttf:text='LightFoxLauncher':x=(w-tw)/2:y=380:fontsize=92:fontcolor=0xeef1f5,drawtext=fontfile=font.ttf:text='零依赖 · 原生 Win32 · 单文件 小于 1 MB':x=(w-tw)/2:y=500:fontsize=34:fontcolor=0x58a6ff,drawtext=fontfile=font.ttf:text='一个用纯 Rust 手写的 Minecraft 启动器：不引第三方库，不登录，不上报。':x=(w-tw)/2:y=580:fontsize=24:fontcolor=0x8b929c" title.png`);
lines.push(`ffmpeg -y -f lavfi -i "color=c=0x0b0c0f:s=1920x1080" -frames:v 1 -filter_complex "drawtext=fontfile=font.ttf:text='把一切打成一个文件夹':x=(w-tw)/2:y=360:fontsize=64:fontcolor=0xeef1f5,drawtext=fontfile=font.ttf:text='客户端、库、资源、Java 运行时 都在 .minecraft 里':x=(w-tw)/2:y=470:fontsize=34:fontcolor=0x58a6ff,drawtext=fontfile=font.ttf:text='复制到任何机器，打开就能玩。':x=(w-tw)/2:y=524:fontsize=34:fontcolor=0x58a6ff,drawtext=fontfile=font.ttf:text='GitHub · shouzugzs/LightFoxLauncher    MPL-2.0':x=(w-tw)/2:y=610:fontsize=24:fontcolor=0x8b929c" end.png`);

// 输入
const inputs = [];
inputs.push(`-r 30 -loop 1 -t ${(DUR_TITLE + MARGIN).toFixed(1)} -i title.png`);
slides.forEach(s => inputs.push(`-r 30 -loop 1 -t ${(DUR_SLIDE + MARGIN).toFixed(1)} -i assets/${s.img}`));
inputs.push(`-r 30 -loop 1 -t ${(DUR_END + MARGIN).toFixed(1)} -i end.png`);

// 各路滤镜
const fc = [];
fc.push(`[0:v]drawtext=fontfile=font.ttf:text='LightFoxLauncher':x=(w-tw)/2:y=380:fontsize=92:fontcolor=0xeef1f5,drawtext=fontfile=font.ttf:text='零依赖 · 原生 Win32 · 单文件 小于 1 MB':x=(w-tw)/2:y=500:fontsize=34:fontcolor=0x58a6ff,fps=30,format=yuv420p[v0]`);

slides.forEach((s, i) => {
  const n = i + 1;
  const frames = Math.round(DUR_SLIDE * FPS);
  let f = `[${n}:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=0x0b0c0f,setsar=1,zoompan=z='min(1.10, zoom+0.0009)':d=${frames}:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=1920x1080:fps=${FPS},drawbox=x=36:y=36:w=iw-72:h=ih-72:t=3:color=0x58a6ff@0.55,drawbox=x=0:y=756:w=iw:h=64:color=0x0b0c0f@0.45:t=64,drawbox=x=0:y=820:w=iw:h=260:color=0x0b0c0f@1.0:t=260`;
  f += `,drawtext=fontfile=font.ttf:text='${s.title}':x=90:y=876:fontsize=54:fontcolor=0x58a6ff:box=1:boxcolor=0x0b0c0f@0.8:boxborderw=14`;
  s.lines.forEach((ln, k) => {
    f += `,drawtext=fontfile=font.ttf:text='${ln}':x=90:y=${940 + k * 42}:fontsize=30:fontcolor=0xd7dce2:box=1:boxcolor=0x0b0c0f@0.8:boxborderw=12`;
  });
  f += `,format=yuv420p[v${n}]`;
  fc.push(f);
});

const last = L + 1;
fc.push(`[${last}:v]drawtext=fontfile=font.ttf:text='把一切打成一个文件夹':x=(w-tw)/2:y=360:fontsize=64:fontcolor=0xeef1f5,drawtext=fontfile=font.ttf:text='客户端、库、资源、Java 运行时 都在 .minecraft 里':x=(w-tw)/2:y=470:fontsize=34:fontcolor=0x58a6ff,drawtext=fontfile=font.ttf:text='复制到任何机器，打开就能玩。':x=(w-tw)/2:y=524:fontsize=34:fontcolor=0x58a6ff,drawtext=fontfile=font.ttf:text='GitHub · shouzugzs/LightFoxLauncher    MPL-2.0':x=(w-tw)/2:y=610:fontsize=24:fontcolor=0x8b929c,fps=30,format=yuv420p[v${last}]`);

// xfade 链式拼接
let chain = '[v0]';
let acc = DUR_TITLE;
for (let i = 1; i <= last; i++) {
  const offset = (acc - XF).toFixed(2);
  chain += `[v${i}]xfade=transition=fade:duration=${XF}:offset=${offset}[x${i}]`;
  if (i < last) chain += `;[x${i}]`;
  acc += dur(i);
}
const finalOut = `[x${last}]`;

const filter = fc.join(';') + ';' + chain;
lines.push(`ffmpeg -y ${inputs.join(' ')} -filter_complex "${filter}" -map "${finalOut}" -c:v libx264 -pix_fmt yuv420p -r ${FPS} -movflags +faststart intro.mp4`);

fs.writeFileSync('run_intro.sh', lines.join('\n') + '\n');
console.log('run_intro.sh written, estimated duration ~' + (acc - XF * last).toFixed(1) + 's');
