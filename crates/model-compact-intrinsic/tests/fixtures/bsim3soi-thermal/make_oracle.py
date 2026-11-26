from pathlib import Path
fields='leff weff leffCV weffCV abulkCVfactor ua ub uc uatemp ubtemp uctemp u0 u0temp vsattemp rds0denom rds0 rth cth rbody cgdo cgso cgeo npeak jbjt jdif jrec jtun vfbb vsdfb vsdth phi sqrtPhi phis3 Xdep0 sqrtXdep0 litl vbi cdep0 k1 k2 vbsc vfb vth0 theta0vb0 thetaRout'.split()
template=r'''
#include "ngspice/ngspice.h"
#include "ngspice/cktdefs.h"
#include "ngspice/smpdefs.h"
#include "ngspice/ifsim.h"
#include "HEADER"
#include "EXT"
#include <stdarg.h>
double CONSTroot2=1.4142135623730951, CONSTvt0=0.02586419;
void *tmalloc(size_t n) { return calloc(1,n); }
void txfree(const void *p) { free((void *)p); }
static void diagnostic(int n, const char *fmt, ...) {
  va_list args; va_start(args,fmt); vfprintf(stderr,fmt,args); va_end(args);
  fputc('\n',stderr); if(n) exit(2);
}
static IFfrontEnd frontend={.IFerrorf=diagnostic};
IFfrontEnd *SPfrontEnd=&frontend;
static CKTnode nodes[256]; static int count=16;
int CKTmkVolt(CKTcircuit *c,CKTnode **out,IFuid id,char *suffix) {
  (void)c;(void)id;(void)suffix; if(count>=256) abort();
  nodes[count].number=count; *out=&nodes[count++]; return 0;
}
int CKTdltNNum(CKTcircuit *c,int i) {(void)c;(void)i;return 0;}
int CKTinst2Node(CKTcircuit *c,void *x,int i,CKTnode **n,IFuid *u) {
  (void)c;(void)x;(void)i;(void)n;(void)u;abort();
}
static double matrix[256][256];
double *SMPmakeElt(SMPmatrix *m,int r,int c) {(void)m;if(r<0||c<0||r>=256||c>=256)abort();return &matrix[r][c];}
extern IFparm FAMILYmPTable[], FAMILYpTable[];
extern int FAMILYmPTSize, FAMILYpTSize;
static IFparm *find(IFparm *table,int n,const char *key) {
  for(int i=0;i<n;i++) if((table[i].dataType&IF_SET)&&!strcmp(table[i].keyword,key))return table+i;
  fprintf(stderr,"unknown %s\n",key);exit(3);
}
int main(int argc,char **argv) {
  FAMILYmodel model={0};
  FAMILYinstance instance={.FAMILYdNode=1,.FAMILYgNode=2,.FAMILYsNode=0,.FAMILYeNode=3,.FAMILYbNodeExt=-1,.FAMILYpNodeExt=-1,.FAMILYtempNodeExt=-1};
  CKTcircuit ckt={0}; ckt.CKTtemp=atof(argv[1]); ckt.CKTnomTemp=300.15;
  model.gen.GENinstances=(GENinstance*)&instance; model.gen.GENmodName="model";
  instance.gen.GENmodPtr=(GENmodel*)&model; instance.gen.GENname="m1";
  for(int i=2;i<argc;i++) {
    int m=argv[i][0]=='m'; char *key=argv[i]+2; char *eq=strchr(key,'=');if(!eq)abort();*eq++=0;
    IFparm *p=m?find(FAMILYmPTable,FAMILYmPTSize,key):find(FAMILYpTable,FAMILYpTSize,key);
    IFvalue v={0};
    switch(p->dataType&IF_VARTYPES) {
      case IF_REAL: v.rValue=atof(eq); break;
      case IF_INTEGER: case IF_FLAG:v.iValue=atoi(eq);break;
      default:abort();
    }
    int rc=m?FAMILYmParam(p->id,&v,(GENmodel*)&model):FAMILYparam(p->id,&v,(GENinstance*)&instance,NULL);
    if(rc){fprintf(stderr,"setter failed %s %d\n",key,rc);return 4;}
  }
  int states=0;
  if(FAMILYsetup(NULL,(GENmodel*)&model,&ckt,&states)) return 5;
  if(FAMILYtemp((GENmodel*)&model,&ckt))return 6;
  struct SIZESTRUCT *p=instance.pParam;
  if(!p)return 7;
#define P(x) printf("value %s %.17g\n",#x,p->FAMILY##x)
#define M(x) printf("value %s %.17g\n",#x,model.FAMILY##x)
#define I(x) printf("value %s %.17g\n",#x,instance.FAMILY##x)
  OUTPUT
  txfree(p);
  return 0;
}
'''
for f in ['fd','dd','pd']:
    family='B3SOI'+f.upper()
    extra='k1eff oxideRatio ahli0 wdiod wdios wdiodCV wdiosCV leffCVb leffCVbg'.split() if f=='pd' else ['csesw','cdesw']
    out='; '.join('P('+x+')' for x in fields+extra)+'; M(eg0); M(vtm); M(csdmin); M(cbox); I(rbodyext); I(drainConductance); I(sourceConductance);'
    if f=='pd':out+=' I(csesw); I(cdesw);'
    code=template.replace('HEADER','b3soi'+f+'def.h').replace('EXT','b3soi'+f+'ext.h').replace('SIZESTRUCT','b3soi'+f+'SizeDependParam').replace('OUTPUT',out).replace('FAMILY',family)
    Path(f'{f}.c').write_text(code)
