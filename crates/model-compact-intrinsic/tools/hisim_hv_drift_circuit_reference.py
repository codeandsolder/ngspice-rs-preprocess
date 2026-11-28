"""HiSIM-HV2 nonlinear physical drift branch, with measured prime-node voltages."""
from itertools import product
from pathlib import Path
import re
import subprocess
import sys
import tempfile

CARDS=(
    'rdrcx=0 rdrqover=0 rdrbb=1',
    'rdrcx=0.35 rdrqover=1e5 rdrbb=2',
    'rdrcx=0.5 rdrqover=3e4 rdrbb=1.5 rdrbbtmp=0.001 rdrmuetmp=1.2 rdrvtmp=0.6 rdrcar=2e-8 rdrmuel=0.3 rdrvmaxw=0.4 rdrvmaxl=0.2 rdrdl1=1e-7 rdrdl2=2e-7',
    'rdrcx=0.2 rdrqover=1e5 rdrbb=1 coadov=1 coqovsm=0',
    'rdrcx=0.2 rdrqover=1e5 rdrbb=1 coovlp=0',
)

def main():
    edge = "--zero-substrate" in sys.argv[2:]
    temperatures = (27.,) if edge else (27.,100.)
    cards = (1,) if edge else range(len(CARDS))
    substrates = (True,) if edge else (False,True)
    geometries = ((3e-6,10e-6,2.,1.),) if edge else ((3e-6,10e-6,2.,1.),(4e-6,30e-6,3.,3.))
    with tempfile.TemporaryDirectory(prefix='hisim-drift-') as tmp, Path(sys.argv[1]).open('w') as f:
        f.write('# polarity temp L W M NF card substrate reversed Vgs Vds Vbs Vsub; physical normalized prime D S, normalized external D/S currents, native normalized channel current\n')
        for polarity,temp,config,substrate in product((1,-1),temperatures,cards,substrates):
            rows=list(product(geometries,(False,True),(5.,8.) if edge else (2.,3.,5.,8.),(1.,5.) if edge else (.1,1.,5.,15.),(-.5,) if edge else (0.,-.5)))
            model=f'{"nmos" if polarity==1 else "pmos"} level=73 version=2.20 cordrift=1 corsrd=0 cosubnode={int(substrate)} nover=1e17 loverld=5e-7 coovlp=1 coadov=0 rsh=10 coisti=0 coisub=0 coiigs=0 cogidl=0 conqs=0 js0d=0 js0s=0 js0swd=0 js0sws=0 {CARDS[config]}'
            if edge: model += " nsubsub=0"
            deck=['* wrapper title','* physical drift circuit',f'.model active {model}',f'.temp {temp}', '.options reltol=1e-9 abstol=1e-16 vntol=1e-11']
            saves=[]
            for i,((length,width,m,nf),reverse,vg,vd,vb) in enumerate(rows):
                dv,gv,bv=(-vd,vg-vd,vb-vd) if reverse else (vd,vg,vb)
                sub=-0.3 if not reverse else -vd-0.3
                deck.extend((f'Vd{i} d{i} 0 {polarity*dv}',f'Vg{i} g{i} 0 {polarity*gv}',
                             f'Vb{i} b{i} 0 {polarity*bv}',f'Vs{i} s{i} 0 0',
                             f'Vsub{i} sub{i} 0 {polarity*sub}'))
                terminal=f' sub{i}' if substrate else ''
                deck.append(f'M{i} d{i} g{i} s{i} b{i}{terminal} active l={length} w={width} m={m} nf={nf} nrd=2 nrs=1')
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
                f.write(' '.join(map(str,(polarity,temp,length,width,m,nf,config,int(substrate),int(reverse),vg,vd,vb,sub,*vals)))+'\n')
if __name__=='__main__':main()
