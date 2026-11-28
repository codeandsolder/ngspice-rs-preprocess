"""Independent source-extracted HiSIM-HV threshold values and gradients."""
from itertools import product
from decimal import Decimal
from pathlib import Path
import math,re,subprocess,sys,tempfile
REVISION='972dbf45db1e47d18bc4fda5d8bb829aff2d0e05'
GRADIENTS=((1,.02,0,.001),(0,1,.04,.0001),(0,.03,.8,-.0002),(.0001,.00002,-.00003,.000002),(0,0,0,-.12),(0,0,0,-.001),(0,0,0,.0001))
def program(root,version):
    prefix='HSMHV' if version==1 else 'HSMHV2'; base='hsmhv' if version==1 else 'hsmhv2'
    src=subprocess.check_output(['git','-C',str(root),'show',f'{REVISION}:src/spicelib/devices/hisimhv{version}/{base}eval.c'],text=True)
    macros=[]
    for name in ('Fn_SU','Fn_SZ'):
        start=src.index('#define '+name+'(');end=re.search(r'(?m)^\s*}[^\S\n]*$',src[start:]).end()+start;macros.append(src[start:end].replace('sqrt (','sqrtl ('))
    start=src.index('T1 = here->'+prefix+'_2qnsub_esi ;',src.index('* Vthp : Vth with pocket.'));end=src.index('* Constants in the equation of Ps0',start);section=src[start:src.rfind('/*',start,end)]
    section=re.sub(r'/\*.*?\*/','',section,flags=re.S)
    names=set(re.findall(r'\b\w+_d(?:V\w+|T)\b',section));decl=';'.join(f'long double {n}=0.' for n in sorted(names))+';'
    # Declare all native scalar temporaries, leaving explicit independent inputs separate.
    inputs={'Vgsz','Vdsz','Vbsz','Vbsz2','Cox','Cox_inv','Cox0_inv','beta','beta_inv','cnstCoxi','Pb2','Pb20','Pb2c','Vfb','Vthq','Weff','small','psia2_dlt','psia_dlt','epsm10','delta0','vth_dlt','C_ESI','TMF1','TMF2'}
    lhs=set(re.findall(r'\b([A-Za-z_]\w*)\s*(?:=|\*=|/=|\+=|-=)\s*[^=]',section));lhs-=inputs|names
    lhs.update(re.findall(r'Fn_(?:SU|SZ)\(\s*(\w+)\s*,',section));lhs-=inputs|names
    # Field initialization follows the source geometry/doping preparation, with
    # zero diffusion reductions, STI, and width corrections on this oracle card.
    maps={
      'here':{'2qnsub_esi':'density','ptovr':'x[6]','nsubc':'bulk','wdplp':'sqrtl(2*C_ESI/(qe*pocket))','wdpl':'sqrtl(2*C_ESI/(qe*nsub))','msc':'0.0007L' if version==1 else '0.L','lgate':'length','wg':'width*1e6L','dVthsm':'0.005L/(length*width*1e12L)'},
      'model':{'lp':'lp','bs1':'0.02L','bs2':'0.4L','vbi':'1.1L','scp21':'0.6L','parl2':'0.05e-6L','sc4':'0.12L','codep':'x[11]','depeta':'0.07L'},
      'pParam':{'pthrou':'x[10]','scp1':'0.1L','scp2':'0.03L','scp3':'0.04e-6L','sc1':'0.1L','sc2':'0.02L','sc3':'0.03e-6L','wfc':'0.3e-9L','wvth0':'0.03L'}
    }
    structs=[]
    for obj,init in maps.items():
        fields=sorted(set(re.findall(r'\b'+obj+r'->'+prefix+r'_(\w+)',section)))
        structs.append('struct {'+' '.join(f'long double {prefix}_{field};' for field in fields)+'} '+obj+'_value={'+','.join(init[field] for field in fields)+'},*'+obj+'=&'+obj+'_value;')
    return f'''#include <math.h>
#include <stdio.h>
#define Fn_Sgn(x) ((x)>=0?1:-1)
#define C_QE 1.6021918e-19L
{chr(10).join(macros)}
int main(void){{long double x[12];while(1){{for(int i=0;i<12;i++)if(scanf("%Lf",&x[i])!=1)return 0;
long double Vgsz=x[0],Vdsz=x[1],Vbsz=x[2],Vbsz2=Vbsz,Cox=x[3],Cox_inv=1.L/Cox,beta=x[4],beta_inv=1.L/beta,Pb2=x[5],cnstCoxi=0.0005L*0.0005L*Cox_inv*Cox_inv;
long double length=x[7],width=x[8],lp=x[9],bulk=1e23L,pocket=5e23L,nsub=length>lp?(bulk*(length-lp)+pocket*lp)/length:pocket+(pocket-bulk)*(lp-length)/lp,qe=1.6021918e-19L,C_ESI=1.034943e-10L,density=2.L*qe*C_ESI*nsub;
long double Pb20=2.L/38.68283L*logl(nsub/1.04e16L),Pb2c=2.L/38.68283L*logl(bulk/1.04e16L),Vfb=-0.7L,Weff=width,Cox0_inv=10e-9L/(8.8541878e-12L*3.9L),Vthq=Pb20+Vfb+sqrtl(density*Pb20)*Cox0_inv;
long double small=1e-25L,psia2_dlt=.005L,psia_dlt=.001L,epsm10=2.220446049250313e-15L,delta0=.005L,vth_dlt=.001L,TMF1=0,TMF2=0;
{chr(10).join(structs)}
{decl}
long double {','.join(n+'=0' for n in sorted(lhs))};
{section.replace('sqrt(', 'sqrtl(').replace('sqrt (','sqrtl (').replace('fabs(', 'fabsl(')}
printf("%.20Lg %.20Lg %.20Lg %.20Lg %.20Lg %.20Lg %.20Lg\\n",dVthLP,dVthSC,dVthW,dVth,Pb20b,Pbsum,Vth);
}}}}
'''
def main():
    root,out=map(Path,sys.argv[1:]);
    with tempfile.TemporaryDirectory(prefix='hisim-threshold-oracle-') as tmp,out.open('w') as f:
        f.write('# family Vgsz Vdsz Vbsz Cox beta Pb2 ptovr L W lp pthrou codep; pocket short narrow total Pb20b Pbsum Vth then four gradients each\n')
        for version in (1,2):
            c,exe=Path(tmp)/'oracle.c',Path(tmp)/'oracle';c.write_text(program(root,version));subprocess.run(['sccache','cc','-std=c11','-O2',str(c),'-lm','-o',str(exe)],check=True)
            cases=[]
            for gate,drain,body,cox,beta,length,width,lp,mode in product((-1.5,.4,3.),(.01,.5,5.),(-1.,-.1,.25),(.002,.0035),(30.,45.),(.8e-6,2e-6),(2e-6,10e-6),(0.,.5e-6),(0,1)):
                pb2=.9;ptovr=0. if length>2*lp else math.log((2*5e23-(5e23-1e23)*length/lp-1e23)/1e23)/beta
                cases.append((gate,drain,body,cox,beta,pb2,ptovr,length,width,lp,.2*mode if version==1 else 0.,mode if version==2 else 0))
            queries=[]
            for case in cases:
                queries.append(case)
                for axis in range(4):
                    for sign in (-1,1):queries.append(tuple(v+sign*(1e-3 if axis==3 else 1e-5)*GRADIENTS[i][axis] if i<7 else v for i,v in enumerate(case)))
            result=subprocess.run([str(exe)],input=''.join(' '.join(map(str,row))+'\n' for row in queries),text=True,capture_output=True,check=True)
            rows=[tuple(map(Decimal,line.split())) for line in result.stdout.splitlines()];assert len(rows)==len(queries)
            for i,case in enumerate(cases):
                values=rows[i*9:i*9+9];gradients=[(values[2*a+2][j]-values[2*a+1][j])/Decimal('0.002' if a==3 else '0.00002') for j in range(7) for a in range(4)];result=(*values[0],*gradients);assert all(map(math.isfinite,result)),(case,result);f.write(' '.join(map(str,(version,*case,*result)))+'\n')
if __name__=='__main__':main()
