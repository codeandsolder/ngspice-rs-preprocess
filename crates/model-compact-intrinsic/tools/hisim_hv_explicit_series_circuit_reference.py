"""HiSIM-HV conventional explicit series branches, with measured prime-node voltages."""
from itertools import product
from pathlib import Path
import re
import subprocess
import sys
import tempfile

CARDS=(
    'cosym=0 rd=0.005 rs=0.003 rdvd=0 rdvg11=0 rdvb=0',
    'cosym=0 rd=0.005 rs=0.003 rdvd=0.015 rdvg11=0.4 rdvg12=5 rdvb=0.2',
    'cosym=1 rd=0.005 rs=0.005 rdvd=0.02 rdvg11=0.8 rdvg12=7 rdvb=0.15 rdtemp1=0.00002 rdtemp2=1e-8 rdvdtemp1=0.00003 rdvdtemp2=1e-8 rds=1.5 rdsp=0.7 rdvds=2 rdvdsp=0.8 rdvdl=0.02 rdvdlp=1.1 rdslp1=0.3 rdict1=0.7 rdslp2=0.8 rdict2=0.2 rdov11=0.4 rdov12=1.2 rdov13=0.6',
)

def main():
    with tempfile.TemporaryDirectory(prefix='hisim-series-') as tmp, Path(sys.argv[1]).open('w') as f:
        f.write('# family corsrd polarity temp L W M NF card substrate reversed Vgs Vds Vbs Vsub; physical normalized prime D S, normalized external D/S currents, native normalized channel current\n')
        for family,mode,polarity,temp,config,substrate in product((1,2),(1,3),(1,-1),(27.,100.),range(len(CARDS)),(False,True)):
            rows=list(product(((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.)),(False,True),(2.,4.,8.),(.1,2.,10.),(0.,-.5)))
            family_options = "cordrift=0 js0d=0 js0s=0 js0swd=0 js0sws=0" if family==2 else "js0=0 js0sw=0"
            model=f'{"nmos" if polarity==1 else "pmos"} level=73 version={"1.24" if family==1 else "2.20"} corsrd={mode} cosubnode={int(substrate)} nover=1e17 loverld=5e-7 lovers=3e-7 coovlp=0 coadov=0 rsh=10 coisti=0 coisub=0 coiigs=0 cogidl=0 conqs=0 {family_options} {CARDS[config]}'
            deck=['* wrapper title','* conventional series circuit',f'.model active {model}',f'.temp {temp}', '.options reltol=1e-9 abstol=1e-16 vntol=1e-11 gmin=1e-16']
            saves=[]
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                dv,gv,bv=(-vd,vg-vd,vb-vd) if reverse else (vd,vg,vb)
                sub=-0.3 if not reverse else -vd-0.3
                deck.extend((f'Vd{i} d{i} 0 {polarity*dv}',f'Vg{i} g{i} 0 {polarity*gv}',
                             f'Vb{i} b{i} 0 {polarity*bv}',f'Vs{i} s{i} 0 0',
                             f'Vsub{i} sub{i} 0 {polarity*sub}'))
                terminal=f' sub{i}' if substrate else ''
                deck.append(f'M{i} d{i} g{i} s{i} b{i}{terminal} active l={length} w={width} m={m} nf={nf} nrd=2 nrs=1 ldrift1=1.2e-6 ldrift2=1.5e-6 ldrift1s=0.8e-6 ldrift2s=1.1e-6')
                saves.extend((f'v(m{i}#drain)',f'v(m{i}#source)',f'@m{i}[ids]',f'i(vd{i})',f'i(vs{i})'))
            deck.extend(('.control','set numdgt=16','save '+' '.join(saves),'op'))
            deck.extend('print '+item for item in saves)
            deck.extend(('quit','.endc','.end'))
            proc=subprocess.run(['ngspice','-b','-n','/dev/stdin'],input='\n'.join(deck)+'\n',text=True,capture_output=True,cwd=tmp,timeout=120)
            if proc.returncode or 'No. of Data Rows : 1' not in proc.stdout:
                raise RuntimeError(proc.stdout+proc.stderr)
            if proc.stderr.strip(): print(proc.stderr,file=sys.stderr)
            values={name:float(value) for name,value in re.findall(r'(?m)^([^\s]+)\s*=\s*([^\s]+)',proc.stdout)}
            assert all(name in values for name in saves),(len(values),proc.stdout[-1000:],proc.stderr[-1000:])
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                sub=-0.3 if not reverse else -vd-0.3
                vals=[polarity*values[f'v(m{i}#drain)'],polarity*values[f'v(m{i}#source)'],
                      -polarity*values[f'i(vd{i})'],-polarity*values[f'i(vs{i})'],values[f'@m{i}[ids]']]
                f.write(' '.join(map(str,(family,mode,polarity,temp,length,width,m,nf,config,int(substrate),int(reverse),vg,vd,vb,sub,*vals)))+'\n')
if __name__=='__main__':main()
